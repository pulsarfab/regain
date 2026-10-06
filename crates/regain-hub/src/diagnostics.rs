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
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

pub const MAX_PAGE: u32 = 32;

pub fn description() -> Value {
    json!({"purpose":"cachedDiagnostics","opensSource":false,"writesEquipment":false,
    "countsSafetyObservations":false,"requiresRevision":true,"deadlineSeconds":5,
    "parameters":{
        "start":{"type":"integer","label":"First output item","description":"Saved membership, channel slot or weather metric index.","default":0,"minimum":0,"maximum":1024},
        "limit":{"type":"integer","label":"Items per page","description":"Maximum cached output items returned in one request.","default":16,"minimum":1,"maximum":MAX_PAGE}
    }})
}

pub(crate) fn page(start: u32, limit: u32, total: u32) -> Result<u32, SourceError> {
    if limit == 0 || limit > MAX_PAGE || start > total {
        return Err(invalid("Output diagnostic page is outside its range"));
    }
    Ok(start.saturating_add(limit).min(total))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
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
}

/// Selected fields only: no backend configuration, connection strings, cached
/// arbitrary vendor text, credentials or credential references are exported.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceHealth {
    pub source: Uuid,
    pub revision: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub transport_connected: bool,
    pub write_uncertain: bool,
    pub lease_count: usize,
    pub error: Option<SourceError>,
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
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyMember {
    pub source: Uuid,
    pub enabled: bool,
    pub policy: SafetyPolicy,
    /// Disabled memberships have no vote. Inactive enabled memberships are
    /// unknown/unsafe; reading diagnostics never starts a policy controller.
    pub decision: Option<Snapshot>,
    pub health: SourceHealth,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeatherMeasurement {
    pub metric: WeatherMetric,
    pub configuration: Measurement,
    pub sample: Reading<WeatherReading>,
    pub sources: Vec<SourceHealth>,
}
