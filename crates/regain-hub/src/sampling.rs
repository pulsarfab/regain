//! Typed property plans and incremental polling shared by Alpaca and COM imports.
//! Transport adapters issue one request at a time; this state owns type checks,
//! conservative sensor ages and same-key retry accounting, not source ownership.
use crate::{
    config::{DeviceType, Readout, WeatherMetric},
    source::{ErrorKind, MAX_SAMPLE_KEYS, SampleBatch, SampleBudget, SourceError, Values},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tokio::time::Instant;

#[derive(Clone, Copy)]
pub enum SampleType {
    Boolean,
    Number,
    Text,
    Strings,
    Int32s,
}
#[derive(Clone)]
pub struct SampleRequest {
    pub key: String,
    pub member: String,
    pub parameters: Values,
    pub value_type: SampleType,
    pub sensor_age: Option<String>,
}
impl SampleRequest {
    pub fn safety() -> Self {
        Self {
            key: "issafe".into(),
            member: "issafe".into(),
            parameters: Values::new(),
            value_type: SampleType::Boolean,
            sensor_age: None,
        }
    }
    pub fn readout(readout: &Readout, observing_conditions: bool) -> Self {
        match readout {
            Readout::Channel { channel, .. } => Self {
                key: readout.sample_key(),
                member: "getswitchvalue".into(),
                parameters: Values::from([("Id".into(), json!(channel))]),
                value_type: SampleType::Number,
                sensor_age: None,
            },
            Readout::Property { property, .. } => Self {
                key: readout.sample_key(),
                member: property.clone(),
                parameters: Values::new(),
                value_type: if property == "issafe" {
                    SampleType::Boolean
                } else {
                    SampleType::Number
                },
                sensor_age: observing_conditions.then(|| property.clone()),
            },
        }
    }
    pub(crate) fn valid_value(&self, value: &Value) -> bool {
        let typed = match self.value_type {
            SampleType::Boolean => value.is_boolean(),
            SampleType::Number => value.as_f64().is_some_and(f64::is_finite),
            SampleType::Text => value.is_string(),
            SampleType::Strings => value
                .as_array()
                .is_some_and(|values| values.iter().all(Value::is_string)),
            SampleType::Int32s => value.as_array().is_some_and(|values| {
                values.iter().all(|value| {
                    value
                        .as_i64()
                        .is_some_and(|value| i32::try_from(value).is_ok())
                })
            }),
        };
        typed && SampleBudget::default().admit(value)
    }
}

pub(crate) struct PropertyPoll {
    samples: Vec<SampleRequest>,
    cursor: usize,
    pending_age: Option<(f64, Instant)>,
    failures: u32,
    attempts: u32,
    safety: bool,
}
pub(crate) struct PreparedSample {
    pub member: String,
    pub parameters: Values,
    sample: SampleRequest,
    needs_age: bool,
    age: f64,
    pub began: Instant,
}
impl PropertyPoll {
    pub fn new(
        device: DeviceType,
        samples: Vec<SampleRequest>,
        attempts: u32,
    ) -> Result<Self, SourceError> {
        let invalid = || SourceError::new(ErrorKind::InvalidValue, "Invalid property sample plan");
        let safety = device == DeviceType::SafetyMonitor;
        let mut keys = BTreeSet::new();
        if samples.len() > MAX_SAMPLE_KEYS
            || attempts == 0
            || safety
                && (samples.len() != 1
                    || samples[0].key != "issafe"
                    || samples[0].member != "issafe"
                    || !matches!(samples[0].value_type, SampleType::Boolean)
                    || !samples[0].parameters.is_empty()
                    || samples[0].sensor_age.is_some())
        {
            return Err(invalid());
        }
        for sample in &samples {
            if sample.key.is_empty()
                || sample.key.len() > 200
                || !keys.insert(&sample.key)
                || !valid_member(&sample.member)
                || sample
                    .sensor_age
                    .as_ref()
                    .is_some_and(|age| !valid_member(age))
                || sample.parameters.len() > 32
                || sample.parameters.iter().any(|(k, v)| {
                    k.is_empty()
                        || k.len() > 64
                        || !k.bytes().all(|b| b.is_ascii_alphanumeric())
                        || !matches!(v, Value::Bool(_) | Value::Number(_))
                            && !v.as_str().is_some_and(|s| s.len() <= 8192)
                })
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            samples,
            cursor: 0,
            pending_age: None,
            failures: 0,
            attempts,
            safety,
        })
    }
    pub fn samples(&self) -> &[SampleRequest] {
        &self.samples
    }
    pub fn prepare(&self) -> Option<PreparedSample> {
        let sample = self.samples.get(self.cursor)?.clone();
        let needs_age = sample.sensor_age.is_some() && self.pending_age.is_none();
        let age = self
            .pending_age
            .map_or(0.0, |(age, at)| age + at.elapsed().as_secs_f64());
        Some(PreparedSample {
            member: if needs_age {
                "timesincelastupdate".into()
            } else {
                sample.member.clone()
            },
            parameters: if needs_age {
                Values::from([(
                    "SensorName".into(),
                    json!(sample.sensor_age.as_deref().map(WeatherMetric::sensor_name)),
                )])
            } else {
                sample.parameters.clone()
            },
            sample,
            needs_age,
            age,
            began: Instant::now(),
        })
    }
    pub fn finish(
        &mut self,
        prepared: PreparedSample,
        result: Result<Value, SourceError>,
        bad_value: SourceError,
    ) -> Result<SampleBatch, SourceError> {
        let mut batch = SampleBatch {
            partial: !self.safety,
            ..SampleBatch::default()
        };
        let sample = prepared.sample;
        match result {
            Ok(value) if prepared.needs_age => {
                if let Some(age) = value.as_f64().filter(|age| age.is_finite() && *age >= 0.0) {
                    self.failures = 0;
                    self.pending_age = Some((age, prepared.began));
                    batch.more = true;
                    return Ok(batch);
                }
                batch.errors.insert(
                    sample.key,
                    SourceError::new(ErrorKind::Unavailable, "Sensor has no valid update time"),
                );
            }
            Ok(value) if sample.valid_value(&value) => {
                batch.ages_seconds.insert(sample.key.clone(), prepared.age);
                batch.values.insert(sample.key, value);
            }
            Ok(_) if self.safety => return Err(bad_value),
            Ok(_) => {
                batch.errors.insert(sample.key, bad_value);
            }
            Err(error) => {
                if self.safety || error.transport_lost {
                    return Err(error);
                }
                if matches!(error.kind, ErrorKind::Transient | ErrorKind::Disconnected) {
                    self.failures += 1;
                    if self.failures < self.attempts {
                        batch.errors.insert(sample.key, error);
                        batch.more = true;
                        return Ok(batch);
                    }
                }
                batch.errors.insert(sample.key, error);
            }
        }
        self.failures = 0;
        self.pending_age = None;
        self.cursor = (self.cursor + 1) % self.samples.len();
        batch.more = self.cursor != 0;
        Ok(batch)
    }
    pub fn restart(&mut self) {
        self.cursor = 0;
        self.pending_age = None;
        self.failures = 0;
    }
}
fn valid_member(member: &str) -> bool {
    !member.is_empty()
        && member.len() <= 64
        && member
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}
