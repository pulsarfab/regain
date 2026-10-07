//! Canonical native camera recovery configuration and frontend metadata.
//! Legacy keys/defaults and acceptance ranges remain unchanged. Proxy cameras
//! must not acquire these native recovery promises merely by republishing them.
use crate::Failure;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy)]
pub enum RecoveryType {
    Integer(u32, u32),
    Number {
        minimum: f64,
        exclusive: bool,
        maximum: f64,
    },
    Boolean,
}
pub struct RecoveryField {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub units: &'static str,
    pub step: f64,
    pub default: Value,
    pub value_type: RecoveryType,
}
impl RecoveryField {
    pub fn accepts(&self, value: &Value) -> bool {
        match self.value_type {
            RecoveryType::Boolean => value.is_boolean(),
            RecoveryType::Integer(min, max) => value
                .as_u64()
                .is_some_and(|v| (min as u64..=max as u64).contains(&v)),
            RecoveryType::Number {
                minimum,
                exclusive,
                maximum,
            } => value.as_f64().is_some_and(|v| {
                v.is_finite() && v <= maximum && if exclusive { v > minimum } else { v >= minimum }
            }),
        }
    }
    pub fn schema(&self) -> Value {
        let mut schema = json!({
            "title":self.label,"description":self.description,"default":self.default,
            "x-regain":{"key":self.key,"label":self.label,"description":self.description,
                "group":"Native camera recovery","units":self.units,"step":self.step,
                "apply":"reconnect","sensitive":false}
        });
        match self.value_type {
            RecoveryType::Boolean => schema["type"] = json!("boolean"),
            RecoveryType::Integer(min, max) => {
                schema["type"] = json!("integer");
                schema["minimum"] = json!(min);
                schema["maximum"] = json!(max);
            }
            RecoveryType::Number {
                minimum,
                exclusive,
                maximum,
            } => {
                schema["type"] = json!("number");
                schema[if exclusive {
                    "exclusiveMinimum"
                } else {
                    "minimum"
                }] = json!(minimum);
                schema["maximum"] = json!(maximum);
            }
        }
        schema
    }
}
macro_rules! recovery {
    ($($field:ident : $ty:ty = $default:expr => ($key:literal, $label:literal, $description:literal, $units:literal, $step:expr, $kind:expr)),+ $(,)?) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct RecoveryOptions { $(#[serde(rename = $key)] pub $field: $ty),+ }
        impl Default for RecoveryOptions {
            fn default() -> Self { Self { $($field:$default),+ } }
        }
        impl RecoveryOptions {
            pub fn fields() -> Vec<RecoveryField> {
                vec![$(RecoveryField {key:$key,label:$label,description:$description,units:$units,step:$step,default:json!($default),value_type:$kind}),+]
            }
            pub fn validate(&self) -> Result<()> {
                let values = serde_json::to_value(self)?;
                for field in Self::fields() {
                    ensure!(field.accepts(&values[field.key]), Failure::Invalid(format!("Invalid camera recovery setting: {}",field.key)));
                }
                Ok(())
            }
            /// Strict hub/frontend schema without a schema/compiler dependency.
            /// Legacy profile loading intentionally still tolerates unknown keys.
            pub fn schema() -> Value {
                let properties: serde_json::Map<String, Value> = Self::fields().into_iter().map(|field| (field.key.into(),field.schema())).collect();
                json!({"type":"object","additionalProperties":false,"default":Self::default(),"properties":properties})
            }
        }
    }
}
use RecoveryType::{Boolean, Integer, Number};
recovery! {
    max_retries: u32 = 3 => ("maxRetries", "Replacement exposures", "Maximum replacement exposures after a recoverable failure. Zero disables replacement exposures.", "retries", 1.0, Integer(0,20)),
    maximum_retry_exposure_seconds: f64 = 30.0 => ("maximumRetryExposureSeconds", "Replacement exposure limit", "Only exposures at or below this duration may be replaced. Same-frame rereads do not start a replacement exposure.", "s", 1.0, Number {minimum:0.0,exclusive:false,maximum:86400.0}),
    reconnect_delay_seconds: f64 = 5.0 => ("reconnectDelaySeconds", "Reconnect delay", "Wait before restoring a camera worker after a recoverable failure or explicit abort.", "s", 0.1, Number {minimum:0.0,exclusive:true,maximum:3600.0}),
    command_timeout_seconds: f64 = 15.0 => ("commandTimeoutSeconds", "Command timeout", "Deadline for a native worker command. Gain, offset and cooler changes include readback within the same deadline.", "s", 1.0, Number {minimum:0.0,exclusive:true,maximum:3600.0}),
    download_timeout_seconds: f64 = 60.0 => ("downloadTimeoutSeconds", "Download timeout", "Deadline for one image download attempt. Native same-frame rereads retain their own bounded attempts.", "s", 1.0, Number {minimum:0.0,exclusive:true,maximum:3600.0}),
    exposure_grace_seconds: f64 = 30.0 => ("exposureGraceSeconds", "Exposure grace", "Additional time beyond the requested exposure for the native worker to report a ready frame.", "s", 1.0, Number {minimum:0.0,exclusive:true,maximum:3600.0}),
    cooling_timeout_seconds: f64 = 300.0 => ("coolingTimeoutSeconds", "Cooling recovery timeout", "Maximum thermal settling time after reconnecting an enabled cooler before a replacement exposure.", "s", 1.0, Number {minimum:0.0,exclusive:true,maximum:3600.0}),
    temperature_tolerance_c: f64 = 2.0 => ("temperatureToleranceC", "Temperature tolerance", "Allowed temperature difference from the pre-failure reading while checking cooling recovery.", "Â°C", 0.1, Number {minimum:0.0,exclusive:true,maximum:3600.0}),
    cooling_stable_samples: u32 = 3 => ("coolingStableSamples", "Stable cooling samples", "Consecutive acceptable temperature and cooler-power observations required during recovery.", "samples", 1.0, Integer(1,60)),
    cooling_sample_seconds: f64 = 2.0 => ("coolingSampleSeconds", "Cooling check interval", "Time between temperature and cooler-power checks while recovering a camera connection.", "s", 0.1, Number {minimum:0.0,exclusive:true,maximum:3600.0}),
    ready_frame_download_retries: u32 = 2 => ("readyFrameDownloadRetries", "SDK ready-frame rereads", "Additional SDK download attempts only while the same exposure still reports a ready frame. No new exposure is started by a reread.", "retries", 1.0, Integer(0,5)),
    direct_read_retries: u32 = 2 => ("directReadRetries", "Direct USB rereads", "Additional direct USB read attempts under the selected camera's capabilities. Retained-frame support is device-specific; this does not grant rereads to proxy cameras.", "retries", 1.0, Integer(0,5)),
    usb_reset_after_failures: u32 = 0 => ("usbResetAfterFailures", "USB reset threshold", "After this many recoverable failures, permit at most one USB reset per capture. Zero disables it; the selected camera must have a verified reset target and required OS permissions.", "failures", 1.0, Integer(0,20)),
    usb_port_cycle: bool = false => ("usbPortCycle", "Linux USB port cycle", "On Linux, use a downstream port power cycle instead of USBDEVFS_RESET when configured USB recovery is triggered.", "", 1.0, Boolean),
}
