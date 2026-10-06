//! Bounded observations of existing controllers. Diagnostics never acquire a
//! source lease, inspect capabilities, refresh a sensor or authorize a write.
use crate::{
    config::{DeviceType, Measurement, Readout, WeatherMetric},
    parameters::SafetyPolicy,
    readout::{ScalarSample, invalid},
    safety::Snapshot,
    source::{SourceError, SourceSnapshot},
    weather::WeatherReading,
};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub const MAX_PAGE: u32 = 32;

pub fn description() -> Value {
    json!({"purpose":"cachedDiagnostics","opensSource":false,"writesEquipment":false,
    "countsSafetyObservations":false,"requiresRevision":true,"deadlineSeconds":5,
    "parameters":{
        "start":{"type":"integer","label":"First output item","description":"Saved membership, channel slot, weather metric or typed property index.","default":0,"minimum":0,"maximum":1024},
        "limit":{"type":"integer","label":"Items per page","description":"Maximum cached output items returned in one request.","default":16,"minimum":1,"maximum":MAX_PAGE}
    }, "focuserProperties": crate::focuser::FocuserProperty::ALL.iter().map(|property|
        json!({"property":property,"valueType":property.value_type(),
            "minimum": match property { crate::focuser::FocuserProperty::Position => Some(0),
                crate::focuser::FocuserProperty::MaxStep | crate::focuser::FocuserProperty::MaxIncrement => Some(1), _ => None },
            "exclusiveMinimum": if *property == crate::focuser::FocuserProperty::StepSize { Some(0) } else { None }
        })).collect::<Vec<_>>(),
    "rotatorProperties": crate::rotator::RotatorProperty::ALL.iter().map(|property|
        json!({"property":property,"valueType":property.value_type(),
            "minimum": if matches!(property, crate::rotator::RotatorProperty::MechanicalPosition
                | crate::rotator::RotatorProperty::Position | crate::rotator::RotatorProperty::TargetPosition) { Some(0.0) } else { None },
            "exclusiveMinimum": if *property == crate::rotator::RotatorProperty::StepSize { Some(0.0) } else { None },
            "maximum": if *property == crate::rotator::RotatorProperty::StepSize { Some(f32::MAX as f64) } else { None },
            "exclusiveMaximum": if matches!(property, crate::rotator::RotatorProperty::MechanicalPosition
                | crate::rotator::RotatorProperty::Position | crate::rotator::RotatorProperty::TargetPosition) { Some(360.0) } else { None },
        })).collect::<Vec<_>>(),
    "filterwheelProperties": crate::filterwheel::FilterWheelProperty::ALL.iter().map(|property|
        json!({"property":property,"valueType":property.value_type(),
            "minimum": if *property == crate::filterwheel::FilterWheelProperty::Position { Some(-1) } else { None },
            "exclusiveMinimum": Option::<i32>::None,
            "maximum": if *property == crate::filterwheel::FilterWheelProperty::Position { Some(crate::filterwheel::MAX_FILTER_SLOTS as i32 - 1) } else { None },
        })).collect::<Vec<_>>(),
    "responseSchema": schemars::generate::SchemaSettings::default()
        .for_serialize().into_generator().into_root_schema_for::<OutputStatus>()})
}

pub(crate) fn page(start: u32, limit: u32, total: u32) -> Result<u32, SourceError> {
    if limit == 0 || limit > MAX_PAGE || start > total {
        return Err(invalid("Output diagnostic page is outside its range"));
    }
    Ok(start.saturating_add(limit).min(total))
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputStatus {
    pub purpose: &'static str,
    pub output: Uuid,
    pub configuration_revision: Uuid,
    /// Local monotonic time, not a UTC date or a remotely comparable clock.
    pub observed_seconds: f64,
    pub device_type: DeviceType,
    pub simulated: bool,
    pub start: u32,
    pub limit: u32,
    pub total: u32,
    pub next_start: Option<u32>,
    pub diagnostics: Diagnostics,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Diagnostics {
    Safety {
        controller_active: bool,
        /// The whole output's decision, including members outside this page.
        is_safe: bool,
        members: Vec<SafetyMember>,
    },
    Switch {
        channels: Vec<SwitchChannel>,
    },
    Weather {
        average_period_hours: f64,
        measurements: Vec<WeatherMeasurement>,
    },
    Focuser {
        health: SourceHealth,
        properties: Vec<FocuserProperty>,
    },
    Rotator {
        health: SourceHealth,
        properties: Vec<RotatorProperty>,
    },
    #[serde(rename = "filterwheel")]
    FilterWheel {
        health: SourceHealth,
        properties: Vec<FilterWheelProperty>,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserProperty {
    pub property: crate::focuser::FocuserProperty,
    pub sample: Reading<crate::focuser::FocuserSample>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotatorProperty {
    pub property: crate::rotator::RotatorProperty,
    pub sample: Reading<crate::rotator::RotatorSample>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FilterWheelProperty {
    pub property: crate::filterwheel::FilterWheelProperty,
    pub sample: Reading<crate::filterwheel::FilterWheelSample>,
}

/// Selected fields only: no backend configuration, connection strings, cached
/// arbitrary vendor text, credentials or credential references are exported.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceHealth {
    pub source: Uuid,
    pub revision: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub transport_connected: bool,
    pub write_uncertain: bool,
    pub lease_count: usize,
    pub error: Option<SourceError>,
    pub polling: crate::source::PollingStatus,
}
impl From<&SourceSnapshot> for SourceHealth {
    fn from(state: &SourceSnapshot) -> Self {
        Self {
            source: state.source,
            revision: state.revision,
            generation: state.generation,
            sequence: state.sequence,
            transport_connected: state.transport_connected,
            write_uncertain: state.write_uncertain,
            lease_count: state.lease_count,
            error: state.error.clone(),
            polling: state.polling.clone(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "camelCase", deny_unknown_fields)]
pub enum Reading<T> {
    Available { reading: T },
    Unavailable { error: SourceError },
}
impl<T> From<Result<T, SourceError>> for Reading<T> {
    fn from(result: Result<T, SourceError>) -> Self {
        match result {
            Ok(reading) => Self::Available { reading },
            Err(error) => Self::Unavailable { error },
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SafetyMember {
    pub source: Uuid,
    pub enabled: bool,
    pub policy: SafetyPolicy,
    /// Disabled memberships have no vote. Inactive enabled memberships are
    /// unknown/unsafe; reading diagnostics never starts a policy controller.
    pub decision: Option<Snapshot>,
    pub health: SourceHealth,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SwitchChannel {
    Removed {
        number: u32,
    },
    Configured {
        #[serde(flatten)]
        channel: Box<ConfiguredSwitchChannel>,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfiguredSwitchChannel {
    pub number: u32,
    pub id: Uuid,
    pub label: String,
    pub readout: Readout,
    pub units: String,
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
    /// Configuration intent only. Live CanWrite and source bounds still
    /// govern every operational write; diagnostics do not probe them.
    pub configured_writable: bool,
    pub maximum_age_seconds: f64,
    pub sample: Reading<ScalarSample>,
    pub health: SourceHealth,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WeatherMeasurement {
    pub metric: WeatherMetric,
    pub configuration: Measurement,
    pub sample: Reading<WeatherReading>,
    pub sources: Vec<SourceHealth>,
}
