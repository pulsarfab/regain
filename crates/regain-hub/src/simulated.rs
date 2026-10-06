//! Explicit simulators. One actor owns all clients' state; never fallback
//! after hardware errors. Runtime replacement restarts safety as unsafe.
use crate::{
    alpaca::SampleRequest,
    config::{DeviceType, WeatherMetric},
    readout::invalid,
    rotator::RotatorProperty,
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
    InvalidMotion,
    StalledMotion,
    StoppedShort,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserState {
    pub absolute: bool,
    pub max_step: i32,
    pub max_increment: i32,
    pub position: i32,
    pub is_moving: bool,
    pub temp_comp_available: bool,
    pub temp_comp: bool,
    pub temperature: f64,
    pub temperature_available: bool,
    pub step_size: f64,
    pub step_size_available: bool,
    pub halt_available: bool,
    pub move_duration_seconds: f64,
}
impl Default for FocuserState {
    fn default() -> Self {
        Self {
            absolute: true,
            max_step: 100000,
            max_increment: 1000,
            position: 50000,
            is_moving: false,
            temp_comp_available: true,
            temp_comp: false,
            temperature: 12.0,
            temperature_available: true,
            step_size: 1.25,
            step_size_available: true,
            halt_available: true,
            move_duration_seconds: 0.2,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserUpdate {
    pub absolute: Option<bool>,
    #[schemars(range(min = 1, max = 2147483647))]
    pub max_step: Option<i32>,
    #[schemars(range(min = 1, max = 2147483647))]
    pub max_increment: Option<i32>,
    #[schemars(range(min = 0, max = 2147483647))]
    pub position: Option<i32>,
    pub is_moving: Option<bool>,
    pub temp_comp_available: Option<bool>,
    pub temp_comp: Option<bool>,
    #[schemars(range(min = -273.15))]
    pub temperature: Option<f64>,
    pub temperature_available: Option<bool>,
    pub step_size: Option<f64>,
    pub step_size_available: Option<bool>,
    pub halt_available: Option<bool>,
    #[schemars(range(min = 0, max = 300))]
    pub move_duration_seconds: Option<f64>,
}
impl FocuserUpdate {
    fn replaces_motion(&self) -> bool {
        self.absolute.is_some()
            || self.max_step.is_some()
            || self.max_increment.is_some()
            || self.position.is_some()
            || self.is_moving.is_some()
    }
    fn apply(self, state: &mut FocuserState) -> Result<(), SourceError> {
        macro_rules! apply { ($($key:ident),*) => { $(if let Some(value) = self.$key { state.$key = value; })* }; }
        apply!(
            absolute,
            max_step,
            max_increment,
            position,
            is_moving,
            temp_comp_available,
            temp_comp,
            temperature,
            temperature_available,
            step_size,
            step_size_available,
            halt_available,
            move_duration_seconds
        );
        if state.max_step <= 0
            || state.max_increment <= 0
            || !(0..=state.max_step).contains(&state.position)
            || state.temp_comp && !state.temp_comp_available
            || !state.temperature.is_finite()
            || state.temperature < -273.15
            || !state.step_size.is_finite()
            || state.step_size <= 0.0
            || !state.move_duration_seconds.is_finite()
            || !(0.0..=300.0).contains(&state.move_duration_seconds)
        {
            return Err(invalid("Invalid simulated focuser state"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotatorUpdate {
    pub can_reverse: Option<bool>,
    pub is_moving: Option<bool>,
    #[schemars(range(min = 0), extend("exclusiveMaximum" = 360))]
    pub mechanical_position: Option<f64>,
    #[schemars(range(min = 0), extend("exclusiveMaximum" = 360))]
    pub position: Option<f64>,
    #[schemars(range(min = 0), extend("exclusiveMaximum" = 360))]
    pub target_position: Option<f64>,
    pub reverse: Option<bool>,
    #[schemars(extend("exclusiveMinimum" = 0))]
    pub step_size: Option<f64>,
    pub step_size_available: Option<bool>,
    pub halt_available: Option<bool>,
    #[schemars(range(min = 0, max = 300))]
    pub move_duration_seconds: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotatorState {
    pub can_reverse: bool,
    pub is_moving: bool,
    pub mechanical_position: f64,
    pub position: f64,
    pub target_position: f64,
    pub reverse: bool,
    pub step_size: f64,
    pub step_size_available: bool,
    pub halt_available: bool,
    pub move_duration_seconds: f64,
}
impl Default for RotatorState {
    fn default() -> Self {
        Self {
            can_reverse: true,
            is_moving: false,
            mechanical_position: 0.0,
            position: 0.0,
            target_position: 0.0,
            reverse: false,
            step_size: 0.02,
            step_size_available: true,
            halt_available: true,
            move_duration_seconds: 0.2,
        }
    }
}
impl RotatorUpdate {
    fn replaces_motion(&self) -> bool {
        self.is_moving.is_some()
            || self.mechanical_position.is_some()
            || self.position.is_some()
            || self.target_position.is_some()
            || self.reverse.is_some()
    }
    fn apply(self, state: &mut RotatorState) -> Result<(), SourceError> {
        macro_rules! apply { ($($key:ident),*) => { $(if let Some(value) = self.$key { state.$key = value; })* }; }
        apply!(
            can_reverse,
            is_moving,
            mechanical_position,
            position,
            target_position,
            reverse,
            step_size,
            step_size_available,
            halt_available,
            move_duration_seconds
        );
        if !state.can_reverse && state.reverse
            || !state.move_duration_seconds.is_finite()
            || !(0.0..=300.0).contains(&state.move_duration_seconds)
        {
            return Err(invalid("Invalid simulated rotator state"));
        }
        for (property, value) in [
            (
                RotatorProperty::MechanicalPosition,
                state.mechanical_position,
            ),
            (RotatorProperty::Position, state.position),
            (RotatorProperty::TargetPosition, state.target_position),
            (RotatorProperty::StepSize, state.step_size),
        ] {
            property
                .decode(&json!(value))
                .map_err(|_| invalid("Invalid simulated rotator state"))?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimulationUpdate {
    /// Simulated raw safety reading. Starts false on each new runtime.
    pub safe: Option<bool>,
    /// Inject channel readings, including read-only sensors. Ordinary switch
    /// commands still enforce write permissions and the same value grid.
    #[serde(default, deserialize_with = "switch_values")]
    #[schemars(with = "BTreeMap<u32, f64>")]
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
    /// Sparse focuser test state. Injecting position/motion/limits replaces a
    /// pending simulated movement; other fields do not stop it.
    pub focuser: Option<FocuserUpdate>,
    /// Sparse rotator test state; coordinate/motion/reversal fields replace a
    /// pending movement. Other controls do not stop it.
    pub rotator: Option<RotatorUpdate>,
}
// Internally tagged commands deserialize through serde's captured content,
// whose map keys do not perform JSON's string-to-integer conversion. Parse the
// wire keys explicitly and reject duplicates/noncanonical aliases before use.
fn switch_values<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> Result<BTreeMap<u32, f64>, D::Error> {
    struct Channels;
    impl<'de> serde::de::Visitor<'de> for Channels {
        type Value = BTreeMap<u32, f64>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("canonical simulated channel keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, f64>()? {
                let id = key.parse::<u32>().map_err(serde::de::Error::custom)?;
                if id > 2 || id.to_string() != key || result.insert(id, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "Invalid or duplicate simulated channel",
                    ));
                }
            }
            Ok(result)
        }
    }
    decoder.deserialize_map(Channels)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focuser: Option<FocuserState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotator: Option<RotatorState>,
}
pub fn description() -> Value {
    let mut controls = BTreeMap::new();
    for device in [
        DeviceType::Switch,
        DeviceType::SafetyMonitor,
        DeviceType::ObservingConditions,
        DeviceType::Focuser,
        DeviceType::Rotator,
    ] {
        let state = SimulatedBackend::new(device, Vec::new()).unwrap().state;
        let mut fields = Vec::new();
        if device == DeviceType::SafetyMonitor {
            fields.push(json!({"path":["safe"],"type":"boolean","label":"Raw simulated safety reading",
                "description":"Feeds normal polling and safety confirmation. It does not directly grant permission.","default":state.safe}));
        }
        if device == DeviceType::Switch {
            for (id, value) in state.switch_values {
                let grid = channel_grid(id).unwrap();
                fields.push(json!({"path":["switchValues",id.to_string()],"type":"number","label":channel_name(id),
                    "description":"Inject a channel reading, including a read-only sensor. The host rounds in-range values to its supported step.",
                    "default":value,"minimum":grid.minimum,"maximum":grid.maximum,"step":grid.step}));
            }
        }
        if device == DeviceType::ObservingConditions {
            for (metric, value) in state.weather {
                let (minimum, maximum, exclusive) = metric.bounds();
                let mut field = json!({"path":["weather",metric.property()],"type":"number","nullable":true,
                    "label":format!("{} ({})",metric.state_name(),metric.unit()),"description":"Inject a weather reading. Mark absent to remove this sensor.","default":value});
                if let Some(minimum) = minimum {
                    field[if exclusive {
                        "exclusiveMinimum"
                    } else {
                        "minimum"
                    }] = json!(minimum);
                }
                if let Some(maximum) = maximum {
                    field["maximum"] = json!(maximum);
                }
                fields.push(field);
            }
        }
        if let Some(focuser) = state.focuser.as_ref() {
            let defaults = serde_json::to_value(focuser).unwrap();
            for (key, label) in [
                ("absolute", "Absolute coordinates"),
                ("isMoving", "Moving"),
                ("tempCompAvailable", "Temperature compensation available"),
                ("tempComp", "Temperature compensation enabled"),
                ("temperatureAvailable", "Temperature available"),
                ("stepSizeAvailable", "Step size available"),
                ("haltAvailable", "Halt supported"),
            ] {
                fields.push(json!({"path":["focuser",key],"type":"boolean","label":label,
                    "description":"Inject explicit focuser test state. Motion/coordinate fields replace any pending simulated move.","default":defaults[key]}));
            }
            for (key, label, minimum) in [
                ("maxStep", "Maximum position (steps)", 1),
                ("maxIncrement", "Maximum move (steps)", 1),
                ("position", "Position (steps)", 0),
            ] {
                fields.push(json!({"path":["focuser",key],"type":"integer","label":label,
                    "description":"Int32 focuser coordinate or limit. Position must remain within MaxStep; changing a limit replaces pending test motion.","default":defaults[key],"minimum":minimum,"maximum":i32::MAX}));
            }
            fields.push(json!({"path":["focuser","temperature"],"type":"number","label":"Temperature (°C)","description":"Injected focuser temperature.","default":focuser.temperature,"minimum":-273.15}));
            fields.push(json!({"path":["focuser","stepSize"],"type":"number","label":"Step size (µm)","description":"Injected physical step size.","default":focuser.step_size,"exclusiveMinimum":0.0}));
            fields.push(json!({"path":["focuser","moveDurationSeconds"],"type":"number","label":"Move duration (s)","description":"Move acknowledges start and completes after this monotonic duration. Stalled motion requires an explicit Halt or simulation update.","default":focuser.move_duration_seconds,"minimum":0.0,"maximum":300.0}));
        }
        if let Some(rotator) = state.rotator.as_ref() {
            let defaults = serde_json::to_value(rotator).unwrap();
            for (key, label) in [
                ("canReverse", "Reversal supported"),
                ("isMoving", "Moving"),
                ("reverse", "Direction reversed"),
                ("stepSizeAvailable", "Step size available"),
                ("haltAvailable", "Halt supported"),
            ] {
                fields.push(json!({"path":["rotator",key],"type":"boolean","label":label,
                    "description":"Inject explicit rotator test state. Coordinate, motion and reversal fields replace pending test motion.","default":defaults[key]}));
            }
            for (key, label) in [
                ("position", "Logical angle (°)"),
                ("mechanicalPosition", "Mechanical angle (°)"),
                ("targetPosition", "Target angle (°)"),
            ] {
                fields.push(json!({"path":["rotator",key],"type":"number","precision":"single","label":label,
                    "description":"Shared source coordinate in [0,360). Position minus mechanical position defines the simulated Sync offset.",
                    "default":defaults[key],"minimum":0.0,"exclusiveMaximum":360.0}));
            }
            fields.push(json!({"path":["rotator","stepSize"],"type":"number","precision":"single","label":"Step size (°)",
                "description":"Positive Single-range angular resolution; optional availability is separate.","default":rotator.step_size,"exclusiveMinimum":0.0}));
            fields.push(json!({"path":["rotator","moveDurationSeconds"],"type":"number","label":"Move duration (s)",
                "description":"Motion completes after this monotonic duration. Disconnect does not Halt. New runtimes reset test coordinates.",
                "default":rotator.move_duration_seconds,"minimum":0.0,"maximum":300.0}));
        }
        let faults = match device {
            DeviceType::Switch => vec![
                Fault::None,
                Fault::ReadError,
                Fault::Timeout,
                Fault::UncertainWrite,
            ],
            DeviceType::SafetyMonitor => vec![
                Fault::None,
                Fault::ReadError,
                Fault::Timeout,
                Fault::InvalidSafety,
            ],
            DeviceType::Focuser | DeviceType::Rotator => vec![
                Fault::None,
                Fault::ReadError,
                Fault::Timeout,
                Fault::UncertainWrite,
                Fault::InvalidMotion,
                Fault::StalledMotion,
                Fault::StoppedShort,
            ],
            _ => vec![Fault::None, Fault::ReadError, Fault::Timeout],
        };
        fields.push(json!({"path":["fault"],"type":"string","label":"Injected fault","description":"Select none to clear a fault. This does not clear an uncertain-write latch.","default":state.fault,"enum":faults}));
        if device != DeviceType::SafetyMonitor {
            fields.push(json!({"path":["sampleAgeSeconds"],"type":"number","label":"Sample age (s)","description":"Age reported for each new simulated observation. Refresh resets it to zero.","default":state.sample_age_seconds,"minimum":0.0,"maximum":86400.0}));
        }
        controls.insert(
            serde_json::to_value(device)
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            fields,
        );
    }
    json!({"schema":schemars::schema_for!(SimulationUpdate), "apply":"immediate",
        "controlsByDeviceType":controls,"revisionCheckedUpdates":true,"deadlineSeconds":30,
        "sourceKinds":["simulated"],"deviceTypes":["switch","safetymonitor","observingconditions","focuser","rotator"],
        "fieldsByDeviceType":{"switch":["switchValues","fault","sampleAgeSeconds"],"safetymonitor":["safe","fault"],"observingconditions":["weather","fault","sampleAgeSeconds"],"focuser":["focuser","fault","sampleAgeSeconds"],"rotator":["rotator","fault","sampleAgeSeconds"]},
        "faultsByDeviceType":{"switch":["none","readError","timeout","uncertainWrite"],"safetymonitor":["none","readError","timeout","invalidSafety"],"observingconditions":["none","readError","timeout"],"focuser":["none","readError","timeout","uncertainWrite","invalidMotion","stalledMotion","stoppedShort"],"rotator":["none","readError","timeout","uncertainWrite","invalidMotion","stalledMotion","stoppedShort"]},
        "persistence":"Test state is shared for this runtime only. A new runtime starts safety unsafe.",
        "uncertainWrites":"Changing a fault does not clear an uncertain-write latch. Disconnect every source lease before retrying commands."})
}
pub struct SimulatedBackend {
    state: SimulationStatus,
    samples: Vec<SampleRequest>,
    connected: bool,
    motion: Option<(tokio::time::Instant, MotionTarget)>,
}
#[derive(Clone, Copy)]
enum MotionTarget {
    Focuser(Option<i32>),
    Rotator { logical: f64, mechanical: f64 },
}
impl SimulatedBackend {
    pub fn new(device_type: DeviceType, samples: Vec<SampleRequest>) -> Result<Self, SourceError> {
        if !matches!(
            device_type,
            DeviceType::Switch
                | DeviceType::SafetyMonitor
                | DeviceType::ObservingConditions
                | DeviceType::Focuser
                | DeviceType::Rotator
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
                focuser: (device_type == DeviceType::Focuser).then(FocuserState::default),
                rotator: (device_type == DeviceType::Rotator).then(RotatorState::default),
            },
            samples,
            connected: false,
            motion: None,
        })
    }
    fn patch(&mut self, update: SimulationUpdate) -> Result<SimulationStatus, SourceError> {
        self.advance_motion();
        let mut next = self.state.clone();
        if update.safe.is_some() && next.device_type != DeviceType::SafetyMonitor
            || !update.switch_values.is_empty() && next.device_type != DeviceType::Switch
            || !update.weather.is_empty() && next.device_type != DeviceType::ObservingConditions
            || update.sample_age_seconds.is_some() && next.device_type == DeviceType::SafetyMonitor
            || update.focuser.is_some() && next.device_type != DeviceType::Focuser
            || update.rotator.is_some() && next.device_type != DeviceType::Rotator
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
                || fault == Fault::UncertainWrite
                    && !matches!(
                        next.device_type,
                        DeviceType::Switch | DeviceType::Focuser | DeviceType::Rotator
                    )
                || matches!(
                    fault,
                    Fault::InvalidMotion | Fault::StalledMotion | Fault::StoppedShort
                ) && !matches!(next.device_type, DeviceType::Focuser | DeviceType::Rotator)
            {
                return Err(invalid(
                    "Injected fault does not match the simulated source class",
                ));
            }
            next.fault = fault;
        }
        let replaces_motion = update
            .focuser
            .as_ref()
            .is_some_and(FocuserUpdate::replaces_motion)
            || update
                .rotator
                .as_ref()
                .is_some_and(RotatorUpdate::replaces_motion);
        if let Some(update) = update.focuser {
            update.apply(next.focuser.as_mut().expect("Validated focuser source"))?;
        }
        if let Some(update) = update.rotator {
            update.apply(next.rotator.as_mut().expect("Validated rotator source"))?;
        }
        self.state = next;
        if replaces_motion {
            self.motion = None;
        }
        Ok(self.effective_state())
    }
    fn effective_state(&self) -> SimulationStatus {
        let mut state = self.state.clone();
        if let Some((completes, target)) = self.motion
            && completes <= tokio::time::Instant::now()
            && state.fault != Fault::StalledMotion
        {
            match target {
                MotionTarget::Focuser(target) => {
                    let focuser = state.focuser.as_mut().expect("Focuser motion");
                    focuser.is_moving = false;
                    if let Some(target) = target {
                        focuser.position = if state.fault == Fault::StoppedShort {
                            if target == 0 { 1 } else { target - 1 }
                        } else {
                            target
                        };
                    }
                }
                MotionTarget::Rotator {
                    logical,
                    mechanical,
                } => {
                    let rotator = state.rotator.as_mut().expect("Rotator motion");
                    rotator.is_moving = false;
                    let short = if state.fault == Fault::StoppedShort {
                        1.0
                    } else {
                        0.0
                    };
                    rotator.position = wrap_angle(logical - short);
                    rotator.mechanical_position = wrap_angle(mechanical - short);
                }
            }
        }
        state
    }
    fn advance_motion(&mut self) {
        self.state = self.effective_state();
        if self
            .state
            .focuser
            .as_ref()
            .is_some_and(|state| !state.is_moving)
            || self
                .state
                .rotator
                .as_ref()
                .is_some_and(|state| !state.is_moving)
        {
            self.motion = None;
        }
    }
    fn write_focuser(&mut self, member: &str, args: &Values) -> Result<(), SourceError> {
        let state = self.state.focuser.as_mut().expect("Focuser state");
        match member {
            "move" => {
                if args.len() != 1 {
                    return Err(invalid("Expected only Position"));
                }
                let position = args
                    .get("Position")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| invalid("Expected Int32 Position"))?;
                let valid = if state.absolute {
                    (0..=state.max_step).contains(&position)
                        && state.position.abs_diff(position) <= state.max_increment as u32
                } else {
                    position.unsigned_abs() <= state.max_increment as u32
                };
                if !valid {
                    return Err(invalid("Simulated move exceeds focuser limits"));
                }
                if state.is_moving {
                    return Err(SourceError::new(
                        ErrorKind::Busy,
                        "Simulated focuser is moving",
                    ));
                }
                state.is_moving = true;
                self.motion = Some((
                    tokio::time::Instant::now()
                        + std::time::Duration::from_secs_f64(state.move_duration_seconds),
                    MotionTarget::Focuser(state.absolute.then_some(position)),
                ));
            }
            "halt" => {
                if !args.is_empty() {
                    return Err(invalid("Unexpected Halt parameters"));
                }
                if !state.halt_available {
                    return Err(unsupported());
                }
                state.is_moving = false;
                self.motion = None;
            }
            "tempcomp" => {
                if args.len() != 1 {
                    return Err(invalid("Expected only TempComp"));
                }
                let enabled = args
                    .get("TempComp")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("Expected boolean TempComp"))?;
                if !state.temp_comp_available {
                    return Err(unsupported());
                }
                state.temp_comp = enabled;
            }
            _ => return Err(unsupported()),
        }
        Ok(())
    }
    fn write_rotator(&mut self, member: &str, args: &Values) -> Result<(), SourceError> {
        let state = self.state.rotator.as_mut().expect("Rotator state");
        match member {
            "halt" => {
                if !args.is_empty() {
                    return Err(invalid("Unexpected Halt parameters"));
                }
                if !state.halt_available {
                    return Err(unsupported());
                }
                state.is_moving = false;
                state.target_position = state.position;
                self.motion = None;
            }
            "reverse" => {
                if args.len() != 1 {
                    return Err(invalid("Expected only Reverse"));
                }
                let enabled = args
                    .get("Reverse")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("Expected boolean Reverse"))?;
                if !state.can_reverse {
                    return Err(unsupported());
                }
                if state.is_moving {
                    return Err(SourceError::new(
                        ErrorKind::Busy,
                        "Simulated rotator is moving",
                    ));
                }
                // Angles describe the configured optics' coordinate direction,
                // not a raw motor encoder. Reversal does not move/relabel them.
                state.reverse = enabled;
            }
            "move" | "moveabsolute" | "movemechanical" | "sync" => {
                if args.len() != 1 {
                    return Err(invalid("Expected only Position"));
                }
                let degrees = args
                    .get("Position")
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite() && value.abs() <= f32::MAX as f64)
                    .ok_or_else(|| invalid("Expected Single-range Position"))?;
                if member != "move" {
                    RotatorProperty::Position
                        .decode(&json!(degrees))
                        .map_err(|_| invalid("Invalid simulated angle"))?;
                }
                if state.is_moving {
                    return Err(SourceError::new(
                        ErrorKind::Busy,
                        "Simulated rotator is moving",
                    ));
                }
                if member == "sync" {
                    state.position = degrees;
                    state.target_position = degrees;
                    return Ok(());
                }
                let offset = state.position - state.mechanical_position;
                let (logical, mechanical) = match member {
                    "movemechanical" => (wrap_angle(degrees + offset), degrees),
                    "moveabsolute" => (degrees, wrap_angle(degrees - offset)),
                    _ => {
                        let logical = wrap_angle(state.position + degrees.rem_euclid(360.0));
                        (logical, wrap_angle(logical - offset))
                    }
                };
                state.target_position = logical;
                state.is_moving =
                    logical != state.position || mechanical != state.mechanical_position;
                self.motion = state.is_moving.then(|| {
                    (
                        tokio::time::Instant::now()
                            + std::time::Duration::from_secs_f64(state.move_duration_seconds),
                        MotionTarget::Rotator {
                            logical,
                            mechanical,
                        },
                    )
                });
            }
            _ => return Err(unsupported()),
        }
        Ok(())
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
                    } else if matches!(
                        self.state.device_type,
                        DeviceType::Focuser | DeviceType::Rotator
                    ) {
                        4
                    } else {
                        3
                    }
                ));
            }
            "connected" if args.is_empty() => return Ok(json!(self.connected)),
            _ => {}
        }
        match self.state.device_type {
            DeviceType::Focuser => {
                if !args.is_empty() {
                    return Err(invalid("Unexpected focuser parameters"));
                }
                let state = self.state.focuser.as_ref().expect("Focuser state");
                Ok(match member {
                    "absolute" => json!(state.absolute),
                    "maxstep" => json!(state.max_step),
                    "maxincrement" => json!(state.max_increment),
                    "tempcompavailable" => json!(state.temp_comp_available),
                    "position" if state.absolute => json!(state.position),
                    "ismoving" if self.state.fault == Fault::InvalidMotion => json!("false"),
                    "ismoving" => json!(state.is_moving),
                    "tempcomp" if state.temp_comp_available => json!(state.temp_comp),
                    "temperature" if state.temperature_available => json!(state.temperature),
                    "stepsize" if state.step_size_available => json!(state.step_size),
                    _ => return Err(unsupported()),
                })
            }
            DeviceType::Rotator => {
                if !args.is_empty() {
                    return Err(invalid("Unexpected rotator parameters"));
                }
                let state = self.state.rotator.as_ref().expect("Rotator state");
                Ok(match member {
                    "canreverse" => json!(state.can_reverse),
                    "ismoving" if self.state.fault == Fault::InvalidMotion => json!("false"),
                    "ismoving" => json!(state.is_moving),
                    "mechanicalposition" => json!(state.mechanical_position),
                    "position" => json!(state.position),
                    "targetposition" => json!(state.target_position),
                    "reverse" => json!(state.reverse),
                    "stepsize" if state.step_size_available => json!(state.step_size),
                    _ => return Err(unsupported()),
                })
            }
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
                    "getswitchname" => json!(channel_name(id)),
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
// Computed simulator coordinates are canonical Single angles. Rounding just
// below a turn to 360 is equivalent to zero; external readings are never repaired.
fn wrap_angle(value: f64) -> f64 {
    let angle = value.rem_euclid(360.0) as f32;
    if angle == 360.0 { 0.0 } else { angle as f64 }
}
fn channel_grid(id: u32) -> Result<Grid, SourceError> {
    match id {
        0 => Grid::new(0.0, 1.0, 1.0),
        1 => Grid::new(0.0, 100.0, 1.0),
        2 => Grid::new(-40.0, 80.0, 0.1),
        _ => Err(invalid("Unknown simulated channel")),
    }
}
fn channel_name(id: u32) -> &'static str {
    match id {
        0 => "Simulation relay",
        1 => "Simulation level",
        _ => "Simulation temperature",
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
        Some(self.effective_state())
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
            self.advance_motion();
            self.check_read().await?;
            self.value(&member, &args)
        })
    }
    fn write(&mut self, member: String, args: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.advance_motion();
            self.check_read().await?;
            if self.state.device_type == DeviceType::Focuser {
                self.write_focuser(&member, &args)?;
                return if self.state.fault == Fault::UncertainWrite {
                    Err(SourceError::uncertain())
                } else {
                    Ok(Value::Null)
                };
            }
            if self.state.device_type == DeviceType::Rotator {
                self.write_rotator(&member, &args)?;
                return if self.state.fault == Fault::UncertainWrite {
                    Err(SourceError::uncertain())
                } else {
                    Ok(Value::Null)
                };
            }
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
            self.advance_motion();
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
