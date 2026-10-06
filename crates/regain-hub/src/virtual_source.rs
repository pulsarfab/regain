//! In-process output composition. The graph is validated before construction;
//! connections use the same controllers and source leases as external clients.
use crate::{
    alpaca::SampleRequest,
    config::{DeviceType, WeatherMetric},
    ipc::{Get, Put},
    readout::invalid,
    runtime::{ClientSession, HubRuntime, OutputConnection},
    safety::Clock,
    source::{Backend, BackendFuture, ErrorKind, SampleBatch, SourceError, Values},
};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock, Weak};
use uuid::Uuid;

pub(crate) type Binding = Arc<OnceLock<Weak<HubRuntime>>>;
pub(crate) struct VirtualBackend {
    binding: Binding,
    output: Uuid,
    kind: DeviceType,
    samples: Vec<SampleRequest>,
    clock: Arc<dyn Clock>,
    simulated: bool,
    client: Option<Arc<ClientSession>>,
}
impl VirtualBackend {
    pub(crate) fn new(
        binding: Binding,
        output: Uuid,
        kind: DeviceType,
        samples: Vec<SampleRequest>,
        clock: Arc<dyn Clock>,
        simulated: bool,
    ) -> Self {
        Self {
            binding,
            output,
            kind,
            samples,
            clock,
            simulated,
            client: None,
        }
    }
    fn connection(&self) -> Result<Arc<OutputConnection>, SourceError> {
        self.client
            .as_ref()
            .ok_or_else(disconnected)?
            .connection(self.output)
    }
    fn close(&mut self) {
        if let Some(client) = self.client.take() {
            client.close();
        }
    }
}
fn disconnected() -> SourceError {
    SourceError::new(ErrorKind::Disconnected, "Virtual output is disconnected")
}
fn unsupported() -> SourceError {
    SourceError::new(
        ErrorKind::Unsupported,
        "Virtual output member is not supported",
    )
}
fn no_args(args: &Values) -> Result<(), SourceError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(invalid("Unexpected virtual output parameters"))
    }
}
fn id(args: &Values, count: usize) -> Result<u32, SourceError> {
    if args.len() != count {
        return Err(invalid("Unexpected virtual channel parameters"));
    }
    args.get("Id")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(|| invalid("Expected virtual channel Id"))
}
fn sensor(args: &Values) -> Result<String, SourceError> {
    if args.len() != 1 {
        return Err(invalid("Expected SensorName"));
    }
    args.get("SensorName")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid("Expected SensorName"))
}
fn metric(member: &str) -> Result<WeatherMetric, SourceError> {
    serde_json::from_value(json!(member)).map_err(|_| unsupported())
}
impl Backend for VirtualBackend {
    fn simulated(&self) -> bool {
        self.simulated
    }
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            if self.client.is_none() {
                let runtime = self
                    .binding
                    .get()
                    .and_then(Weak::upgrade)
                    .ok_or_else(disconnected)?;
                self.client = Some(runtime.client());
            }
            self.client
                .as_ref()
                .expect("Created client")
                .connect(self.output)
                .await
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.close();
            Ok(())
        })
    }
    fn reset(&mut self) {
        self.close();
    }
    fn read(&mut self, member: String, args: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            let connection = self.connection()?;
            if member == "interfaceversion" {
                no_args(&args)?;
                return Ok(json!(if self.kind == DeviceType::ObservingConditions {
                    2
                } else {
                    3
                }));
            }
            if member == "connected" {
                no_args(&args)?;
                return Ok(json!(true));
            }
            if member == "connecting" {
                no_args(&args)?;
                return Ok(json!(false));
            }
            if member == "devicestate" {
                no_args(&args)?;
                return connection.get(Get::DeviceState {}).await;
            }
            let get = match self.kind {
                DeviceType::SafetyMonitor if member == "issafe" => {
                    no_args(&args)?;
                    Get::IsSafe {}
                }
                DeviceType::Switch => {
                    if member == "maxswitch" {
                        no_args(&args)?;
                        Get::MaxSwitch {}
                    } else {
                        let id = id(&args, 1)?;
                        match member.as_str() {
                            "getswitch" => Get::GetSwitch { id },
                            "getswitchvalue" => Get::GetSwitchValue { id },
                            "getswitchname" => Get::GetSwitchName { id },
                            "getswitchdescription" => Get::GetSwitchDescription { id },
                            "canwrite" => Get::CanWrite { id },
                            "canasync" => Get::CanAsync { id },
                            "statechangecomplete" => Get::StateChangeComplete { id },
                            "minswitchvalue" => Get::MinSwitchValue { id },
                            "maxswitchvalue" => Get::MaxSwitchValue { id },
                            "switchstep" => Get::SwitchStep { id },
                            _ => return Err(unsupported()),
                        }
                    }
                }
                DeviceType::ObservingConditions => match member.as_str() {
                    "timesincelastupdate" => Get::TimeSinceLastUpdate {
                        sensor: sensor(&args)?,
                    },
                    "sensordescription" => {
                        return Ok(json!(
                            connection.weather()?.sensor_description(&sensor(&args)?)?
                        ));
                    }
                    "averageperiod" => {
                        no_args(&args)?;
                        Get::AveragePeriod {}
                    }
                    _ => {
                        no_args(&args)?;
                        return Ok(json!(connection.weather()?.read(metric(&member)?)?.value));
                    }
                },
                _ => return Err(unsupported()),
            };
            connection.get(get).await
        })
    }
    fn write(&mut self, member: String, args: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            let connection = self.connection()?;
            let put = match (self.kind, member.as_str()) {
                (DeviceType::Switch, "cancelasync") => Put::CancelAsync { id: id(&args, 1)? },
                (DeviceType::Switch, "setswitchvalue") => Put::SetSwitchValue {
                    id: id(&args, 2)?,
                    value: args
                        .get("Value")
                        .and_then(Value::as_f64)
                        .ok_or_else(|| invalid("Expected Value"))?,
                },
                (DeviceType::Switch, "setswitch") => Put::SetSwitch {
                    id: id(&args, 2)?,
                    state: args
                        .get("State")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| invalid("Expected State"))?,
                },
                (DeviceType::ObservingConditions, "averageperiod") if args.len() == 1 => {
                    Put::AveragePeriod {
                        hours: args
                            .get("AveragePeriod")
                            .and_then(Value::as_f64)
                            .ok_or_else(|| invalid("Expected AveragePeriod"))?,
                    }
                }
                (DeviceType::ObservingConditions, "refresh") => {
                    no_args(&args)?;
                    Put::Refresh {}
                }
                _ => return Err(unsupported()),
            };
            connection.put(put).await?;
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async { Ok(self.sample().await?.values) })
    }
    fn sample(&mut self) -> BackendFuture<'_, SampleBatch> {
        Box::pin(async {
            let connection = self.connection()?;
            let mut batch = SampleBatch::default();
            if self.kind == DeviceType::SafetyMonitor {
                let snapshot = connection.safety()?.snapshot();
                if snapshot.is_safe
                    && snapshot.endpoints.values().any(|endpoint| {
                        endpoint.phase != crate::safety::Phase::FreshSafe
                            || !endpoint.recovery_confirmed
                    })
                {
                    // Permission retained under an inner grace policy is not a
                    // new successful observation. Preserve the outer policy's
                    // grace/expiry rules and block aggregate recovery from it.
                    return Err(SourceError::transient());
                }
                batch
                    .values
                    .insert("issafe".into(), json!(snapshot.is_safe));
                batch.safety_observed_at = snapshot.safe_observed_at;
                if let Some(at) = snapshot.safe_observed_at {
                    batch.ages_seconds.insert(
                        "issafe".into(),
                        self.clock.now().saturating_sub(at).as_secs_f64(),
                    );
                }
                return Ok(batch);
            }
            for request in &self.samples {
                let result = match self.kind {
                    DeviceType::Switch if request.member == "getswitchvalue" => connection
                        .switch()?
                        .sample(id(&request.parameters, 1)?)
                        .map(|s| (s.value, s.age_seconds)),
                    DeviceType::ObservingConditions => metric(&request.member)
                        .and_then(|m| connection.weather()?.read(m))
                        .map(|s| (s.value, s.age_seconds)),
                    _ => Err(unsupported()),
                };
                match result {
                    Ok((value, age)) => {
                        batch.values.insert(request.key.clone(), json!(value));
                        batch.ages_seconds.insert(request.key.clone(), age);
                    }
                    Err(error) => {
                        batch.errors.insert(request.key.clone(), error);
                    }
                }
            }
            Ok(batch)
        })
    }
    fn refresh(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async { self.connection()?.weather()?.refresh().await })
    }
}
impl Drop for VirtualBackend {
    fn drop(&mut self) {
        self.close();
    }
}
