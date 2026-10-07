//! Native-only configuration. Recovery metadata comes from the camera core;
//! republished cameras do not inherit native retry or retained-frame promises.
use crate::{config::MAX_LABEL_CHARS, parameters::FieldError};
use regain_core::{RecoveryOptions, Selection};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, de::Error};
use serde_json::Value;
use std::{borrow::Cow, collections::BTreeMap};

/// Legacy profiles accept unknown extension keys. New hub configuration rejects
/// them while using exactly the same sparse defaults and runtime value type.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct CameraRecovery(pub RecoveryOptions);

impl<'de> Deserialize<'de> for CameraRecovery {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = BTreeMap::<String, Value>::deserialize(deserializer)?;
        let fields = RecoveryOptions::fields();
        if let Some(key) = values
            .keys()
            .find(|key| !fields.iter().any(|f| f.key == *key))
        {
            return Err(D::Error::custom(format!(
                "Unknown camera recovery setting: {key}"
            )));
        }
        serde_json::from_value(Value::Object(values.into_iter().collect()))
            .map(Self)
            .map_err(D::Error::custom)
    }
}
impl JsonSchema for CameraRecovery {
    fn schema_name() -> Cow<'static, str> {
        "CameraRecovery".into()
    }
    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        serde_json::from_value(RecoveryOptions::schema()).expect("Recovery schema is an object")
    }
}
impl CameraRecovery {
    pub fn validate(&self) -> Vec<FieldError> {
        let values = serde_json::to_value(&self.0).expect("Recovery options serialize");
        RecoveryOptions::fields()
            .into_iter()
            .filter(|field| !field.accepts(&values[field.key]))
            .map(|field| {
                FieldError::new(
                    field.key,
                    "range",
                    format!(
                        "{} is outside the native camera recovery range",
                        field.label
                    ),
                )
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeCameraConfig {
    /// Exact camera model name. The source identity selects its physical serial;
    /// discovery order is never used to choose a replacement camera.
    #[schemars(length(min = 1, max = MAX_LABEL_CHARS))]
    pub model: String,
    /// Permit SDK fallback only when explicitly selecting a direct USB source.
    /// Fallback does not retain the direct backend's reread capabilities.
    #[serde(default)]
    pub sdk_fallback: bool,
    /// Native capture recovery. These settings do not apply to Alpaca or COM inputs.
    #[serde(default)]
    pub recovery: CameraRecovery,
}
impl NativeCameraConfig {
    pub fn validate(&self, direct: bool) -> Vec<FieldError> {
        let mut errors = Vec::new();
        if self.model.trim().is_empty()
            || self.model.chars().count() > MAX_LABEL_CHARS
            || self.model.chars().any(char::is_control)
        {
            errors.push(FieldError::new(
                "model",
                "identity",
                "Select an exact camera model name",
            ));
        }
        if self.sdk_fallback && !direct {
            errors.push(FieldError::new(
                "sdkFallback",
                "type",
                "SDK fallback is only available for direct USB sources",
            ));
        }
        errors.extend(self.recovery.validate().into_iter().map(|mut error| {
            error.path = format!("recovery.{}", error.path);
            error
        }));
        errors
    }
    /// Convert a validated native source without discovery or worker launch.
    /// The caller supplies the source's immutable physical identity/backend.
    pub fn selection(&self, serial: &str, direct: bool) -> Selection {
        Selection {
            name: self.model.clone(),
            serial: Some(serial.into()),
            direct,
            sdk_fallback: self.sdk_fallback,
            recovery: self.recovery.0.clone(),
        }
    }
}
