use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
pub enum Failure {
    Invalid(String),
    Cancelled,
    Worker {
        message: String,
        code: Option<i32>,
    },
    /// A dispatched control has no known acknowledgement. Retire its worker;
    /// retrying the command or capture could repeat an already applied write.
    UncertainControl {
        message: String,
        code: Option<i32>,
    },
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m)
            | Self::Worker { message: m, .. }
            | Self::UncertainControl { message: m, .. } => f.write_str(m),
            Self::Cancelled => f.write_str("Exposure aborted"),
        }
    }
}
impl std::error::Error for Failure {}
pub fn invalid(message: impl Into<String>) -> anyhow::Error {
    Failure::Invalid(message.into()).into()
}
pub fn retryable(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<Failure>() {
        Some(Failure::Invalid(_) | Failure::Cancelled | Failure::UncertainControl { .. }) => false,
        Some(Failure::Worker { code: Some(c), .. }) => {
            matches!(c, 1 | 2 | 4 | 5 | 11 | 12 | 15 | 16)
        }
        _ => true,
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Exposure {
    pub width: u32,
    pub height: u32,
    pub bin: u32,
    pub x: u32,
    pub y: u32,
    pub microseconds: u64,
    pub dark: bool,
}
impl Exposure {
    pub fn bytes(&self) -> Result<usize> {
        let count = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .and_then(|pixels| pixels.checked_mul(2))
            .ok_or_else(|| invalid("Invalid frame size"))?;
        ensure!(
            count > 0 && count <= 512 * 1024 * 1024,
            Failure::Invalid("Invalid frame size".into())
        );
        Ok(count as usize)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Control {
    #[serde(rename = "type")]
    pub kind: i32,
    pub min: i64,
    pub max: i64,
    pub value: i64,
    pub writable: bool,
}
pub use crate::recovery::RecoveryOptions;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub name: String,
    pub serial: Option<String>,
    pub direct: bool,
    pub sdk_fallback: bool,
    pub recovery: RecoveryOptions,
}
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub info: Value,
    pub serial: Option<String>,
    pub sdk_version: String,
    pub backend: String,
    pub sdk_fallback: bool,
    pub phase: String,
    pub error: Option<String>,
    pub controls: BTreeMap<i32, Control>,
    pub values: BTreeMap<i32, i64>,
    /// Acknowledged values, separate from queued desired settings. Monotonic
    /// timestamps belong to this process and never enter the status JSON.
    #[serde(skip)]
    pub observations: BTreeMap<i32, ControlObservation>,
    pub connected: bool,
    pub control_connection_available: bool,
    pub sdk_exposure_state: Option<i64>,
    pub sdk_error_code: Option<i32>,
    pub process_id: Option<u32>,
    pub white_balance_capabilities: Value,
    pub white_balance: Option<crate::white_balance::Settings>,
}
pub type SharedStatus = Arc<Mutex<Status>>;
pub type Diagnostic = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlObservation {
    pub value: i64,
    pub observed_at: tokio::time::Instant,
}
/// Worker-relative age; never transfer a process-local monotonic timestamp.
/// Receivers must also account for the request/response transit time.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlObservationReply {
    pub value: i64,
    pub age_seconds: f64,
}
impl ControlObservationReply {
    pub fn normalize(&self, request_started: tokio::time::Instant) -> Result<ControlObservation> {
        Ok(ControlObservation {
            value: self.value,
            observed_at: self.observed_at(request_started)?,
        })
    }
    /// Conservatively include all IPC/worker time by subtracting the reported
    /// age from request admission, never from response receipt. Process-local
    /// monotonic clock epochs do not need to agree across the pipe.
    pub fn observed_at(
        &self,
        request_started: tokio::time::Instant,
    ) -> Result<tokio::time::Instant> {
        let age = std::time::Duration::try_from_secs_f64(self.age_seconds)
            .map_err(|_| invalid("Invalid control observation age"))?;
        request_started
            .checked_sub(age)
            .ok_or_else(|| invalid("Unrepresentable control observation time"))
    }
}
#[derive(Clone)]
pub struct Frame {
    pub exposure: Exposure,
    pub pixels: Arc<[u8]>,
    pub metadata: Value,
}

pub fn validate_exposure(
    info: &Value,
    controls: &BTreeMap<i32, Control>,
    e: &Exposure,
) -> Result<()> {
    e.bytes()?;
    let bins = info["bins"]
        .as_array()
        .ok_or_else(|| invalid("Camera binning is unavailable"))?;
    let w = info["width"].as_u64().unwrap_or(0);
    let h = info["height"].as_u64().unwrap_or(0);
    let exp = controls
        .get(&1)
        .ok_or_else(|| invalid("Exposure control unavailable"))?;
    let alignment = info["originAlignment"]
        .as_u64()
        .or_else(|| info["originAlignmentX"].as_u64())
        .unwrap_or(1)
        .max(1);
    let align_y = info["originAlignmentY"]
        .as_u64()
        .unwrap_or(if info["originAlignment"].is_number() {
            2
        } else {
            1
        })
        .max(1);
    ensure!(e.bin>0 && bins.iter().any(|b|b.as_u64()==Some(e.bin as u64)) && e.width.is_multiple_of(8) && e.height.is_multiple_of(2) &&
        (u64::from(e.x)+u64::from(e.width))*u64::from(e.bin)<=w && (u64::from(e.y)+u64::from(e.height))*u64::from(e.bin)<=h &&
        u64::from(e.width)*u64::from(e.bin)>=info["minimumWidth"].as_u64().unwrap_or(8) && u64::from(e.height)*u64::from(e.bin)>=info["minimumHeight"].as_u64().unwrap_or(2) &&
        u64::from(e.x)*u64::from(e.bin)%alignment==0 && u64::from(e.y)*u64::from(e.bin)%align_y==0 &&
        e.microseconds>0 && e.microseconds>=exp.min.max(0) as u64 && e.microseconds<=exp.max.max(0) as u64,
        Failure::Invalid("Exposure or ROI is outside camera capabilities (width multiple of 8, height multiple of 2; see camera alignment)".into()));
    Ok(())
}

/// Validate against the SDK's ROI rules when fallback is explicitly permitted.
/// The worker's validate command decides whether it must switch before exposing.
pub fn validate_capture(
    info: &Value,
    controls: &BTreeMap<i32, Control>,
    e: &Exposure,
    sdk_fallback: bool,
) -> Result<()> {
    if !sdk_fallback {
        return validate_exposure(info, controls, e);
    }
    let mut info = info.clone();
    info["minimumWidth"] = serde_json::json!(8);
    info["minimumHeight"] = serde_json::json!(2);
    info["originAlignment"] = serde_json::json!(1);
    info["originAlignmentX"] = serde_json::json!(1);
    info["originAlignmentY"] = serde_json::json!(1);
    validate_exposure(&info, controls, e)
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::Instant;

    #[test]
    fn worker_age_normalization_includes_transit_and_rejects_invalid_times() {
        let request = Instant::now();
        let reply = ControlObservationReply {
            value: -100,
            age_seconds: 20.25,
        };
        let observed = reply.observed_at(request).unwrap();
        let received = request + Duration::from_secs(5);
        assert_eq!(
            received.duration_since(observed),
            Duration::from_secs_f64(25.25)
        );
        for invalid_age in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
            assert!(
                ControlObservationReply {
                    value: 0,
                    age_seconds: invalid_age
                }
                .observed_at(request)
                .is_err()
            );
        }
        assert_eq!(
            ControlObservationReply {
                value: 0,
                age_seconds: 0.0
            }
            .observed_at(request)
            .unwrap(),
            request
        );
        for invalid in [
            serde_json::json!({"value":true,"ageSeconds":0}),
            serde_json::json!({"value":0,"ageSeconds":"0"}),
            serde_json::json!({"value":0}),
            serde_json::json!({"value":0,"ageSeconds":0,"timestamp":"private clock epoch"}),
        ] {
            assert!(serde_json::from_value::<ControlObservationReply>(invalid).is_err());
        }
    }
}
