//! Explicit scalar simulators. One actor owns all clients' state; never fallback
//! after hardware errors. Runtime replacement restarts safety as unsafe.
use crate::{
    alpaca::SampleRequest,
    config::{DeviceType, WeatherMetric},
    readout::invalid,
    source::{Backend, BackendFuture, ErrorKind, SampleBatch, SourceError, Values},
    switch::Grid,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Fault {
    #[default]
    None,
    ReadError,
    Timeout,
    InvalidSafety,
    UncertainWrite,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimulationUpdate {
    /// Simulated raw safety reading. Starts false on each new runtime.
    pub safe: Option<bool>,
    /// Inject channel readings, including read-only sensors. Ordinary switch
    /// commands still enforce write permissions and the same value grid.
    #[serde(default)]
    #[schemars(extend("maxProperties" = 3))]
    pub switch_values: BTreeMap<u32, f64>,
    /// Update weather readings; null removes a simulated sensor.
    #[serde(default)]
    #[schemars(extend("maxProperties" = 13))]
    pub weather: BTreeMap<WeatherMetric, Option<f64>>,
    /// Failure to inject. "none" clears a fault; omitted leaves it unchanged.
    pub fault: Option<Fault>,
    /// Fixed age reported for each new simulated scalar observation. Refresh
    /// represents a fresh sensor acquisition and resets this to zero.
    #[schemars(range(min = 0, max = 86400))]
    pub sample_age_seconds: Option<f64>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationStatus {
    pub device_type: DeviceType,
    pub safe: bool,
    pub switch_values: BTreeMap<u32, f64>,
    pub weather: BTreeMap<WeatherMetric, Option<f64>>,
    pub fault: Fault,
    pub sample_age_seconds: f64,
}
pub fn description() -> Value {
    json!({"schema":schemars::schema_for!(SimulationUpdate), "apply":"immediate",
        "sourceKinds":["simulated"],"deviceTypes":["switch","safetymonitor","observingconditions"],
        "fieldsByDeviceType":{"switch":["switchValues","fault","sampleAgeSeconds"],"safetymonitor":["safe","fault"],"observingconditions":["weather","fault","sampleAgeSeconds"]},
        "faultsByDeviceType":{"switch":["none","readError","timeout","uncertainWrite"],"safetymonitor":["none","readError","timeout","invalidSafety"],"observingconditions":["none","readError","timeout"]},
        "persistence":"Test state is shared for this runtime only. A new runtime starts safety unsafe.",
        "uncertainWrites":"Changing a fault does not clear an uncertain-write latch. Disconnect every source lease before retrying commands."})
}
pub struct SimulatedBackend {
    state: SimulationStatus,
    samples: Vec<SampleRequest>,
    connected: bool,
}
impl SimulatedBackend {
    pub fn new(device_type: DeviceType, samples: Vec<SampleRequest>) -> Result<Self, SourceError> {
        if !matches!(
            device_type,
            DeviceType::Switch | DeviceType::SafetyMonitor | DeviceType::ObservingConditions
        ) {
            return Err(unsupported());
        }
        use WeatherMetric::*;
        let weather = [
            (CloudCover, 0.0),
            (DewPoint, 2.0),
            (Humidity, 50.0),
            (Pressure, 1013.0),
            (RainRate, 0.0),
            (SkyBrightness, 0.0),
            (SkyQuality, 21.0),
            (SkyTemperature, -15.0),
            (StarFwhm, 2.0),
            (Temperature, 12.0),
            (WindDirection, 180.0),
            (WindGust, 3.0),
            (WindSpeed, 2.0),
        ]
        .into_iter()
        .map(|(key, value)| (key, Some(value)))
        .collect();
        Ok(Self {
            state: SimulationStatus {
                device_type,
                safe: false,
                switch_values: BTreeMap::from([(0, 0.0), (1, 0.0), (2, 12.0)]),
                weather,
                fault: Fault::None,
                sample_age_seconds: 0.0,
            },
            samples,
            connected: false,
        })
    }
    fn patch(&mut self, update: SimulationUpdate) -> Result<SimulationStatus, SourceError> {
        let mut next = self.state.clone();
        if update.safe.is_some() && next.device_type != DeviceType::SafetyMonitor
            || !update.switch_values.is_empty() && next.device_type != DeviceType::Switch
            || !update.weather.is_empty() && next.device_type != DeviceType::ObservingConditions
            || update.sample_age_seconds.is_some() && next.device_type == DeviceType::SafetyMonitor
        {
            return Err(invalid("Simulator controls do not match this source class"));
        }
        if let Some(value) = update.safe {
            next.safe = value;
        }
        for (id, value) in update.switch_values {
            next.switch_values
                .insert(id, channel_grid(id)?.quantize(value)?);
        }
        for (metric, value) in update.weather {
            if value.is_some_and(|value| !metric.accepts(value)) {
                return Err(invalid("Invalid simulated weather reading"));
            }
            next.weather.insert(metric, value);
        }
        if let Some(age) = update.sample_age_seconds {
            if !age.is_finite() || !(0.0..=86400.0).contains(&age) {
                return Err(invalid("Invalid simulated sample age"));
            }
            next.sample_age_seconds = age;
        }
        if let Some(fault) = update.fault {
            if fault == Fault::InvalidSafety && next.device_type != DeviceType::SafetyMonitor
                || fault == Fault::UncertainWrite && next.device_type != DeviceType::Switch
            {
                return Err(invalid(
                    "Injected fault does not match the simulated source class",
                ));
            }
            next.fault = fault;
        }
        self.state = next;
        Ok(self.state.clone())
    }
    async fn check_read(&self) -> Result<(), SourceError> {
        if !self.connected {
            return Err(SourceError::new(
                ErrorKind::Disconnected,
                "Simulated source is disconnected",
            ));
        }
        match self.state.fault {
            Fault::ReadError => Err(SourceError::new(
                ErrorKind::Unavailable,
                "Injected simulated read failure",
            )),
            Fault::Timeout => std::future::pending().await,
            _ => Ok(()),
        }
    }
    fn value(&self, member: &str, args: &Values) -> Result<Value, SourceError> {
        match member {
            "interfaceversion" if args.is_empty() => {
                return Ok(json!(
                    if self.state.device_type == DeviceType::ObservingConditions {
                        2
                    } else {
                        3
                    }
                ));
            }
            "connected" if args.is_empty() => return Ok(json!(self.connected)),
            _ => {}
        }
        match self.state.device_type {
            DeviceType::SafetyMonitor if member == "issafe" && args.is_empty() => {
                Ok(if self.state.fault == Fault::InvalidSafety {
                    json!(1)
                } else {
                    json!(self.state.safe)
                })
            }
            DeviceType::Switch => {
                if member == "maxswitch" && args.is_empty() {
                    return Ok(json!(3));
                }
                let id = channel_id(args, 1)?;
                let grid = channel_grid(id)?;
                Ok(match member {
                    "canwrite" => json!(id != 2),
                    "minswitchvalue" => json!(grid.minimum),
                    "maxswitchvalue" => json!(grid.maximum),
                    "switchstep" => json!(grid.step),
                    "getswitchname" => json!(match id {
                        0 => "Simulation relay",
                        1 => "Simulation level",
                        _ => "Simulation temperature",
                    }),
                    "getswitchdescription" => {
                        json!("Explicit simulated channel; no hardware is controlled")
                    }
                    "getswitchvalue" => json!(self.state.switch_values[&id]),
                    "getswitch" => json!(self.state.switch_values[&id] != grid.minimum),
                    _ => return Err(unsupported()),
                })
            }
            DeviceType::ObservingConditions => {
                let name = if matches!(member, "sensordescription" | "timesincelastupdate") {
                    if args.len() != 1 {
                        return Err(invalid("Expected SensorName"));
                    }
                    args.get("SensorName")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("Expected SensorName"))?
                } else {
                    if !args.is_empty() {
                        return Err(invalid("Unexpected weather parameters"));
                    }
                    member
                };
                let metric: WeatherMetric =
                    serde_json::from_value(json!(name)).map_err(|_| unsupported())?;
                let value = self.state.weather[&metric].ok_or_else(unsupported)?;
                Ok(match member {
                    "sensordescription" => json!("Explicit simulated weather sensor"),
                    "timesincelastupdate" => json!(self.state.sample_age_seconds),
                    _ => json!(value),
                })
            }
            _ => Err(unsupported()),
        }
    }
}
fn unsupported() -> SourceError {
    SourceError::new(
        ErrorKind::Unsupported,
        "This simulated interface member is not supported",
    )
}
fn channel_grid(id: u32) -> Result<Grid, SourceError> {
    match id {
        0 => Grid::new(0.0, 1.0, 1.0),
        1 => Grid::new(0.0, 100.0, 1.0),
        2 => Grid::new(-40.0, 80.0, 0.1),
        _ => Err(invalid("Unknown simulated channel")),
    }
}
fn channel_id(args: &Values, count: usize) -> Result<u32, SourceError> {
    if args.len() != count {
        return Err(invalid("Unexpected simulated channel parameters"));
    }
    args.get("Id")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid("Invalid simulated channel ID"))
}
impl Backend for SimulatedBackend {
    fn simulated(&self) -> bool {
        true
    }
    fn simulation_status(&self) -> Option<SimulationStatus> {
        Some(self.state.clone())
    }
    fn update_simulation(
        &mut self,
        update: SimulationUpdate,
    ) -> Result<SimulationStatus, SourceError> {
        self.patch(update)
    }
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.connected = true;
            Ok(())
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.connected = false;
            Ok(())
        })
    }
    fn reset(&mut self) {
        self.connected = false;
    }
    fn read(&mut self, member: String, args: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.check_read().await?;
            self.value(&member, &args)
        })
    }
    fn write(&mut self, member: String, args: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.check_read().await?;
            if self.state.device_type != DeviceType::Switch {
                return Err(unsupported());
            }
            let id = channel_id(&args, 2)?;
            let grid = channel_grid(id)?;
            if id == 2 {
                return Err(SourceError::new(
                    ErrorKind::Unsupported,
                    "Simulated sensor is read-only",
                ));
            }
            let value = match member.as_str() {
                "setswitchvalue" => args
                    .get("Value")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| invalid("Expected switch Value"))?,
                "setswitch" => {
                    if args
                        .get("State")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| invalid("Expected switch State"))?
                    {
                        grid.maximum
                    } else {
                        grid.minimum
                    }
                }
                _ => return Err(unsupported()),
            };
            self.state.switch_values.insert(id, grid.quantize(value)?);
            if self.state.fault == Fault::UncertainWrite {
                return Err(SourceError::uncertain());
            }
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async { Ok(self.sample().await?.values) })
    }
    fn sample(&mut self) -> BackendFuture<'_, SampleBatch> {
        Box::pin(async {
            self.check_read().await?;
            let mut batch = SampleBatch::default();
            for sample in &self.samples {
                match self.value(&sample.member, &sample.parameters) {
                    Ok(value) => {
                        batch.values.insert(sample.key.clone(), value);
                        batch
                            .ages_seconds
                            .insert(sample.key.clone(), self.state.sample_age_seconds);
                    }
                    Err(error) => {
                        batch.errors.insert(sample.key.clone(), error);
                    }
                }
            }
            Ok(batch)
        })
    }
    fn refresh(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.check_read().await?;
            self.state.sample_age_seconds = 0.0;
            Ok(())
        })
    }
}
