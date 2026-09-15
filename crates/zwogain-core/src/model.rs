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
    Worker { message: String, code: Option<i32> },
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) | Self::Worker { message: m, .. } => f.write_str(m),
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
        Some(Failure::Invalid(_) | Failure::Cancelled) => false,
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
        let count = u64::from(self.width) * u64::from(self.height) * 2;
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RecoveryOptions {
    pub max_retries: u32,
    pub maximum_retry_exposure_seconds: f64,
    pub reconnect_delay_seconds: f64,
    pub command_timeout_seconds: f64,
    pub download_timeout_seconds: f64,
    pub exposure_grace_seconds: f64,
    pub cooling_timeout_seconds: f64,
    pub temperature_tolerance_c: f64,
    pub cooling_stable_samples: u32,
    pub cooling_sample_seconds: f64,
    pub ready_frame_download_retries: u32,
    pub direct_read_retries: u32,
}
impl Default for RecoveryOptions {
    fn default() -> Self {
        Self {
            max_retries: 3,
            maximum_retry_exposure_seconds: 30.,
            reconnect_delay_seconds: 5.,
            command_timeout_seconds: 15.,
            download_timeout_seconds: 60.,
            exposure_grace_seconds: 30.,
            cooling_timeout_seconds: 300.,
            temperature_tolerance_c: 2.,
            cooling_stable_samples: 3,
            cooling_sample_seconds: 2.,
            ready_frame_download_retries: 2,
            direct_read_retries: 2,
        }
    }
}
impl RecoveryOptions {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_retries <= 20
                && self.ready_frame_download_retries <= 5
                && self.direct_read_retries <= 5
                && (1..=60).contains(&self.cooling_stable_samples),
            Failure::Invalid("Invalid retry limits".into())
        );
        ensure!(
            self.maximum_retry_exposure_seconds.is_finite()
                && (0.0..=86400.).contains(&self.maximum_retry_exposure_seconds),
            Failure::Invalid("Invalid replacement exposure limit".into())
        );
        for v in [
            self.reconnect_delay_seconds,
            self.command_timeout_seconds,
            self.download_timeout_seconds,
            self.exposure_grace_seconds,
            self.cooling_timeout_seconds,
            self.temperature_tolerance_c,
            self.cooling_sample_seconds,
        ] {
            ensure!(
                v.is_finite() && v > 0. && v <= 3600.,
                Failure::Invalid("Invalid recovery timeout or tolerance".into())
            );
        }
        Ok(())
    }
}
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
    pub connected: bool,
    pub control_connection_available: bool,
    pub sdk_exposure_state: Option<i64>,
    pub sdk_error_code: Option<i32>,
    pub process_id: Option<u32>,
}
pub type SharedStatus = Arc<Mutex<Status>>;
pub type Diagnostic = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;
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
