//! Explicit, bounded setup inspection through the existing source actor.
//! Results describe observations, not write authorization or cached safety votes.
use crate::{
    config::{DeviceType, SourceBackend, SourceConfig, WeatherMetric},
    readout::{SourceLease, invalid, unavailable},
    safety::Clock,
    source::{ConnectionInfo, ErrorKind, SourceError, SourceHandle, Values},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

pub const MAX_CHANNEL_PAGE: u32 = 8;
pub const INSPECTION_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_TEXT_CHARS: usize = 1024;

pub fn description() -> Value {
    json!({"purpose":"setupOnly","opensSource":true,"writesEquipment":false,
    "deadlineSeconds":INSPECTION_TIMEOUT.as_secs(),
    "parameters":{
        "start":{"type":"integer","label":"First source channel","description":"Upstream Switch channel index. Use zero for other classes.","default":0,"minimum":0,"maximum":i16::MAX},
        "limit":{"type":"integer","label":"Channels per page","description":"Maximum Switch channels to inspect in one request.","default":4,"minimum":1,"maximum":MAX_CHANNEL_PAGE}
    }})
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Probe {
    Observed { value: Value },
    Unsupported { error: SourceError },
    Unavailable { error: SourceError },
}
impl Probe {
    fn value(&self) -> Option<&Value> {
        match self {
            Self::Observed { value } => Some(value),
            _ => None,
        }
    }
    fn result(result: Result<Value, SourceError>, kind: ValueType) -> Self {
        match result {
            Ok(value) if kind.accepts(&value) => Self::Observed { value },
            Ok(_) => Self::Unavailable {
                error: unavailable("Invalid source capability value"),
            },
            Err(error) if error.kind == ErrorKind::Unsupported => Self::Unsupported { error },
            Err(error) => Self::Unavailable { error },
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueType {
    Boolean,
    Number,
    Text,
    SwitchCount,
    Age,
}
impl ValueType {
    fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Boolean => value.is_boolean(),
            Self::Number => value.as_f64().is_some_and(f64::is_finite),
            Self::Text => value.as_str().is_some_and(|v| {
                v.chars().count() <= MAX_TEXT_CHARS && !v.chars().any(char::is_control)
            }),
            Self::SwitchCount => value.as_u64().is_some_and(|v| v <= i16::MAX as u64),
            Self::Age => value.as_f64().is_some_and(|v| v.is_finite() && v >= 0.0),
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: u32,
    pub name: Probe,
    pub description: Probe,
    pub can_write: Probe,
    pub minimum: Probe,
    pub maximum: Probe,
    pub step: Probe,
    /// Unknown or inconsistent ranges must not become suggested writable config.
    pub range_valid: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Property {
    pub property: String,
    pub value_type: ValueType,
    pub unit: Option<String>,
    pub reading: Probe,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<Probe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<Probe>,
    pub scalar_mapping: bool,
}
#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Capabilities {
    Switch {
        count: Probe,
        channels: Vec<Channel>,
        next_start: Option<u32>,
    },
    Safety {
        is_safe: Probe,
    },
    Weather {
        measurements: Vec<Property>,
    },
    Native {
        properties: Vec<Property>,
    },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub purpose: &'static str,
    pub source: Uuid,
    pub configuration_revision: Uuid,
    pub generation: Uuid,
    pub device_type: DeviceType,
    pub connection: Option<ConnectionInfo>,
    pub simulation: Option<bool>,
    pub started_seconds: f64,
    pub completed_seconds: f64,
    pub capabilities: Capabilities,
}

struct Reader {
    lease: SourceLease,
    generation: Uuid,
}
impl Reader {
    fn verify(&self) -> Result<(), SourceError> {
        let state = self.lease.source.snapshot();
        if !state.transport_connected || state.generation != self.generation {
            return Err(unavailable(
                "Source connection changed during capability inspection",
            ));
        }
        Ok(())
    }
    async fn read(
        &self,
        member: &str,
        parameters: Values,
        kind: ValueType,
    ) -> Result<Probe, SourceError> {
        self.verify()?;
        let result = self
            .lease
            .source
            .read_fenced(self.lease.id, member, parameters, Some(self.generation))
            .await;
        self.verify()?;
        // Stop on backoff hints rather than issuing additional requests during
        // an upstream Retry-After interval. Discovery never retries a request.
        if let Err(error) = &result
            && error.retry_after.is_some()
        {
            return Err(error.clone());
        }
        Ok(Probe::result(result, kind))
    }
    async fn plain(&self, member: &str, kind: ValueType) -> Result<Probe, SourceError> {
        self.read(member, Values::new(), kind).await
    }
    async fn switch(&self, start: u32, limit: u32) -> Result<Capabilities, SourceError> {
        let count = self.plain("maxswitch", ValueType::SwitchCount).await?;
        let Some(total) = count.value().and_then(Value::as_u64) else {
            return Ok(Capabilities::Switch {
                count,
                channels: Vec::new(),
                next_start: None,
            });
        };
        let total = total as u32;
        if start > total || start == total && total != 0 {
            return Err(invalid(
                "Switch inspection start is outside the channel range",
            ));
        }
        let end = start.saturating_add(limit).min(total);
        let mut channels = Vec::new();
        for id in start..end {
            let parameters = Values::from([("Id".into(), json!(id))]);
            let name = self
                .read("getswitchname", parameters.clone(), ValueType::Text)
                .await?;
            let description = self
                .read("getswitchdescription", parameters.clone(), ValueType::Text)
                .await?;
            let can_write = self
                .read("canwrite", parameters.clone(), ValueType::Boolean)
                .await?;
            let minimum = self
                .read("minswitchvalue", parameters.clone(), ValueType::Number)
                .await?;
            let maximum = self
                .read("maxswitchvalue", parameters.clone(), ValueType::Number)
                .await?;
            let step = self
                .read("switchstep", parameters, ValueType::Number)
                .await?;
            let range_valid = match (
                minimum.value().and_then(Value::as_f64),
                maximum.value().and_then(Value::as_f64),
                step.value().and_then(Value::as_f64),
            ) {
                (Some(min), Some(max), Some(step)) => {
                    crate::switch::Grid::new(min, max, step).is_ok()
                }
                _ => false,
            };
            channels.push(Channel {
                id,
                name,
                description,
                can_write,
                minimum,
                maximum,
                step,
                range_valid,
            });
        }
        Ok(Capabilities::Switch {
            count,
            channels,
            next_start: (end < total).then_some(end),
        })
    }
    async fn weather(&self) -> Result<Capabilities, SourceError> {
        use WeatherMetric::*;
        let mut measurements = Vec::new();
        for metric in [
            CloudCover,
            DewPoint,
            Humidity,
            Pressure,
            RainRate,
            SkyBrightness,
            SkyQuality,
            SkyTemperature,
            StarFwhm,
            Temperature,
            WindDirection,
            WindGust,
            WindSpeed,
        ] {
            let property = metric.property();
            let parameters = Values::from([("SensorName".into(), json!(property))]);
            let description = self
                .read("sensordescription", parameters.clone(), ValueType::Text)
                .await?;
            let age_seconds = self
                .read("timesincelastupdate", parameters, ValueType::Age)
                .await?;
            let reading = self.plain(&property, ValueType::Number).await?;
            measurements.push(Property {
                property,
                value_type: ValueType::Number,
                unit: Some(metric.unit().into()),
                reading,
                description: Some(description),
                age_seconds: Some(age_seconds),
                scalar_mapping: true,
            });
        }
        Ok(Capabilities::Weather { measurements })
    }
}

/// Construction/config validation do no probing. Setup explicitly requests this
/// temporary lease, using the source's normal externally-managed/owned policy.
pub(crate) async fn inspect(
    config: &SourceConfig,
    device_type: DeviceType,
    source: Arc<SourceHandle>,
    clock: &dyn Clock,
    start: u32,
    limit: u32,
) -> Result<Inspection, SourceError> {
    if limit == 0 || limit > MAX_CHANNEL_PAGE || start > i16::MAX as u32 {
        return Err(invalid("Invalid capability inspection page"));
    }
    if start != 0 && device_type != DeviceType::Switch {
        return Err(invalid("Only Switch inspection supports channel pages"));
    }
    if !matches!(config.backend, SourceBackend::Native { .. })
        && !matches!(
            device_type,
            DeviceType::Switch | DeviceType::SafetyMonitor | DeviceType::ObservingConditions
        )
    {
        return Err(SourceError::new(
            ErrorKind::Unsupported,
            "This source class has no setup inspection adapter yet",
        ));
    }
    tokio::time::timeout(INSPECTION_TIMEOUT, async {
        let started_seconds = clock.now().as_secs_f64();
        let lease = SourceLease::acquire(source).await?;
        let mut status = lease.source.status();
        let connected = loop {
            status.borrow_and_update();
            let state = lease.source.snapshot();
            if state.transport_connected {
                break state;
            }
            if let Some(error) = state.error
                && error.kind != ErrorKind::Connecting
            {
                return Err(error);
            }
            status
                .changed()
                .await
                .map_err(|_| unavailable("Source stopped during capability inspection"))?;
        };
        let reader = Reader {
            lease,
            generation: connected.generation,
        };
        let mut simulation = connected.simulated.then_some(true);
        let capabilities = if let SourceBackend::Native { device, .. } = config.backend {
            let identity = reader
                .lease
                .source
                .read_fenced(
                    reader.lease.id,
                    "identity",
                    Values::new(),
                    Some(reader.generation),
                )
                .await?;
            reader.verify()?;
            simulation = identity["simulation"].as_bool();
            let mut properties = Vec::new();
            for (property, _, boolean) in crate::native::properties(device) {
                let kind = if *boolean {
                    ValueType::Boolean
                } else {
                    ValueType::Number
                };
                let reading = reader.plain(property, kind).await?;
                properties.push(Property {
                    property: (*property).into(),
                    value_type: kind,
                    unit: (*property == "temperature").then(|| "°C".into()),
                    reading,
                    description: None,
                    age_seconds: None,
                    scalar_mapping: !boolean,
                });
            }
            Capabilities::Native { properties }
        } else {
            match device_type {
                DeviceType::Switch => reader.switch(start, limit).await?,
                DeviceType::SafetyMonitor => Capabilities::Safety {
                    is_safe: reader.plain("issafe", ValueType::Boolean).await?,
                },
                DeviceType::ObservingConditions => reader.weather().await?,
                _ => unreachable!("Validated inspection class"),
            }
        };
        reader.verify()?;
        Ok(Inspection {
            purpose: "setupOnly",
            source: config.id,
            configuration_revision: connected.revision,
            generation: connected.generation,
            device_type,
            connection: connected.connection_info,
            simulation,
            started_seconds,
            completed_seconds: clock.now().as_secs_f64(),
            capabilities,
        })
    })
    .await
    .map_err(|_| unavailable("Source capability inspection deadline expired"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_and_unbounded_values_never_become_observed_capabilities() {
        for (kind, value) in [
            (ValueType::Boolean, json!(1)),
            (ValueType::Number, json!("1")),
            (ValueType::Text, json!("x".repeat(MAX_TEXT_CHARS + 1))),
            (ValueType::Text, json!("bad\nlabel")),
            (ValueType::SwitchCount, json!(32768)),
            (ValueType::SwitchCount, json!(-1)),
            (ValueType::SwitchCount, json!(2.5)),
            (ValueType::Age, json!(-1)),
        ] {
            assert!(matches!(
                Probe::result(Ok(value), kind),
                Probe::Unavailable { .. }
            ));
        }
        assert!(matches!(
            Probe::result(Ok(json!(false)), ValueType::Boolean),
            Probe::Observed { .. }
        ));
        assert!(matches!(
            Probe::result(Ok(json!(32767)), ValueType::SwitchCount),
            Probe::Observed { .. }
        ));
    }
}
