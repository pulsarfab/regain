//! A parameter is declared once: Rust type/default, UI metadata and validation.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldError {
    pub path: String,
    pub code: String,
    pub message: String,
}
impl FieldError {
    pub fn new(path: impl Into<String>, code: &str, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Parameter {
    pub key: String,
    pub value_type: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub group: &'static str,
    pub units: &'static str,
    pub default: Value,
    pub minimum: f64,
    pub maximum: f64,
    pub step: f64,
    pub apply: &'static str,
    pub sensitive: bool,
}

pub trait ParameterSet: Serialize {
    fn parameters() -> Vec<Parameter>;
    fn validate(&self) -> Vec<FieldError>;
    fn schema() -> Value {
        let properties: serde_json::Map<String, Value> = Self::parameters()
            .into_iter()
            .map(|p| {
                (
                    p.key.clone(),
                    json!({"type":p.value_type,"title":p.label,
                "description":p.description,"default":p.default,
                "minimum":p.minimum,"maximum":p.maximum,"x-regain":p}),
                )
            })
            .collect();
        json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
            "type":"object","additionalProperties":false,"properties":properties})
    }
}

fn camel_case(field: &str) -> String {
    let mut upper = false;
    field
        .chars()
        .filter_map(|c| {
            if c == '_' {
                upper = true;
                None
            } else if upper {
                upper = false;
                Some(c.to_ascii_uppercase())
            } else {
                Some(c)
            }
        })
        .collect()
}

macro_rules! parameters {
    ($name:ident, $group:literal, { $($field:ident : $ty:ty = $default:expr =>
        ($label:literal, $description:literal, $units:literal, $min:expr, $max:expr, $step:expr)),* $(,)? }) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(default, rename_all = "camelCase", deny_unknown_fields)]
        pub struct $name { $(pub $field: $ty,)* }
        impl Default for $name {
            fn default() -> Self { Self { $($field: $default,)* } }
        }
        impl ParameterSet for $name {
            fn parameters() -> Vec<Parameter> { vec![$(Parameter {
                key: camel_case(stringify!($field)),
                value_type: if stringify!($ty) == "f64" { "number" } else { "integer" },
                label: $label, description: $description, group: $group, units: $units,
                default: json!($default), minimum: $min, maximum: $max, step: $step,
                apply: "reconnect", sensitive: false,
            }),*] }
            fn validate(&self) -> Vec<FieldError> {
                let mut errors = Vec::new();
                $(if !(self.$field as f64).is_finite() || !(($min)..=($max)).contains(&(self.$field as f64)) {
                    errors.push(FieldError::new(camel_case(stringify!($field)), "range",
                        format!("{} must be between {} and {} {}", $label, $min, $max, $units)));
                })*
                errors
            }
        }
    }
}

parameters!(PollPolicy, "Source polling", {
    poll_seconds: f64 = 30.0 => ("Check interval", "Time between source polling cycles.", "s", 0.1, 3600.0, 0.1),
    request_timeout_seconds: f64 = 1.0 => ("Request timeout", "Deadline for one source request; does not extend safety evidence lifetime.", "s", 0.05, 60.0, 0.05),
    attempts_per_cycle: u32 = 3 => ("Attempts per cycle", "Total attempts including the first request. Only read operations are retried.", "attempts", 1.0, 10.0, 1.0),
    initial_backoff_seconds: f64 = 0.5 => ("Initial retry delay", "Initial exponential retry delay, before equal jitter.", "s", 0.05, 300.0, 0.05),
    backoff_multiplier: f64 = 2.0 => ("Retry multiplier", "Multiplier after consecutive transport failures.", "", 1.0, 10.0, 0.1),
    backoff_cap_seconds: f64 = 30.0 => ("Maximum retry delay", "Cap before jitter. A server Retry-After may require a longer wait; evidence still expires.", "s", 0.05, 3600.0, 0.1)
});

parameters!(SafetyPolicy, "Safety policy", {
    confirmation_seconds: f64 = 30.0 => ("Confirmation interval", "Minimum interval between counted observations for this membership; faster shared polling cannot accelerate confirmation.", "s", 0.1, 3600.0, 0.1),
    failed_cycles_to_unsafe: u32 = 3 => ("Failed checks before unsafe", "Withdraw established permission after this many exhausted checks. Maximum safe age applies independently.", "checks", 1.0, 1000.0, 1.0),
    unsafe_readings_to_unsafe: u32 = 1 => ("Unsafe readings before unsafe", "One withdraws permission immediately. Larger values only retain previously established permission within its age limit.", "readings", 1.0, 1000.0, 1.0),
    safe_readings_to_safe: u32 = 3 => ("Safe readings before recovery", "Consecutive safe observations required in addition to the recovery hold.", "readings", 1.0, 1000.0, 1.0),
    maximum_safe_age_seconds: f64 = 90.0 => ("Maximum safe age", "Hard lifetime of safe evidence, measured from request start. Retries and status reads never extend it.", "s", 0.1, 3600.0, 0.1),
    return_to_safe_hold_seconds: f64 = 10.0 => ("Recovery hold", "Minimum duration of a safe run, beginning when the first safe response arrives. A new observation must complete the hold.", "s", 0.0, 3600.0, 0.1)
});

impl PollPolicy {
    pub fn validate_timing(&self) -> Vec<FieldError> {
        let mut errors = self.validate();
        if self.backoff_cap_seconds < self.initial_backoff_seconds {
            errors.push(FieldError::new(
                "backoffCapSeconds",
                "timing",
                "Retry cap must be at least the initial delay",
            ));
        }
        errors
    }
}
impl SafetyPolicy {
    pub fn validate_source(&self, poll: &PollPolicy) -> Vec<FieldError> {
        let mut errors = self.validate();
        if poll.poll_seconds + poll.request_timeout_seconds >= self.maximum_safe_age_seconds {
            errors.push(FieldError::new(
                "maximumSafeAgeSeconds",
                "timing",
                "Safe age must exceed the source check interval plus request timeout",
            ));
        }
        if self.confirmation_seconds >= self.maximum_safe_age_seconds {
            errors.push(FieldError::new(
                "confirmationSeconds",
                "timing",
                "Confirmation interval must be shorter than the safe evidence lifetime",
            ));
        }
        errors
    }
    pub fn nominal_recovery_seconds(&self) -> f64 {
        let lower_bound = ((self.safe_readings_to_safe.saturating_sub(1)) as f64
            * self.confirmation_seconds)
            .max(self.return_to_safe_hold_seconds);
        // A getter cannot complete the hold between eligible observations.
        (lower_bound / self.confirmation_seconds).ceil() * self.confirmation_seconds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contract<T: ParameterSet + Default>() {
        let defaults = serde_json::to_value(T::default()).unwrap();
        assert_eq!(defaults.as_object().unwrap().len(), T::parameters().len());
        for p in T::parameters() {
            assert_eq!(defaults[&p.key], p.default, "{} default diverged", p.key);
            assert_eq!(T::schema()["properties"][&p.key]["default"], p.default);
            assert!(!p.description.is_empty());
        }
        assert!(T::default().validate().is_empty());
    }
    #[test]
    fn metadata_defaults_and_wire_keys_cannot_drift() {
        contract::<PollPolicy>();
        contract::<SafetyPolicy>();
    }
    #[test]
    fn timing_and_nonfinite_values_are_rejected() {
        let mut safety = SafetyPolicy::default();
        assert_eq!(safety.nominal_recovery_seconds(), 60.0);
        safety.maximum_safe_age_seconds = 31.0;
        assert!(!safety.validate_source(&PollPolicy::default()).is_empty());
        safety.maximum_safe_age_seconds = f64::NAN;
        assert!(
            safety
                .validate()
                .iter()
                .any(|e| e.path == "maximumSafeAgeSeconds")
        );
        assert!(serde_json::from_value::<SafetyPolicy>(json!({"safeReadingsToSafe":1.5})).is_err());
        assert!(serde_json::from_value::<SafetyPolicy>(json!({"typo":1})).is_err());
    }
}
