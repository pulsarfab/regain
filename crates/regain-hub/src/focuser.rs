//! Typed focuser operations over the shared source actor. A session is bound to
//! one transport generation; reconnect explicitly to adopt a replacement device.
use crate::{
    readout::{invalid, unavailable},
    source::{ErrorKind, SourceError, SourceHandle, Values},
    typed_source::TypedSourceSession,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum FocuserProperty {
    Absolute,
    MaxStep,
    MaxIncrement,
    TempCompAvailable,
    Position,
    IsMoving,
    TempComp,
    Temperature,
    StepSize,
}
impl FocuserProperty {
    pub const ALL: [Self; 9] = [
        Self::Absolute,
        Self::MaxStep,
        Self::MaxIncrement,
        Self::TempCompAvailable,
        Self::Position,
        Self::IsMoving,
        Self::TempComp,
        Self::Temperature,
        Self::StepSize,
    ];
    pub fn member(self) -> &'static str {
        match self {
            Self::Absolute => "absolute",
            Self::MaxStep => "maxstep",
            Self::MaxIncrement => "maxincrement",
            Self::TempCompAvailable => "tempcompavailable",
            Self::Position => "position",
            Self::IsMoving => "ismoving",
            Self::TempComp => "tempcomp",
            Self::Temperature => "temperature",
            Self::StepSize => "stepsize",
        }
    }
    pub fn value_type(self) -> &'static str {
        match self {
            Self::Absolute | Self::TempCompAvailable | Self::IsMoving | Self::TempComp => "boolean",
            Self::MaxStep | Self::MaxIncrement | Self::Position => "integer",
            _ => "number",
        }
    }
    pub fn sample_request(self) -> crate::sampling::SampleRequest {
        crate::sampling::SampleRequest {
            key: self.member().into(),
            member: self.member().into(),
            parameters: Values::new(),
            value_type: if self.value_type() == "boolean" {
                crate::sampling::SampleType::Boolean
            } else {
                crate::sampling::SampleType::Number
            },
            sensor_age: None,
        }
    }
    pub fn decode(self, value: &Value) -> Result<FocuserValue, SourceError> {
        let decoded = match self.value_type() {
            "boolean" => value.as_bool().map(|value| FocuserValue::Boolean { value }),
            "integer" => value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .filter(|value| {
                    if self == Self::Position {
                        *value >= 0
                    } else {
                        *value > 0
                    }
                })
                .map(|value| FocuserValue::Integer { value }),
            _ => value
                .as_f64()
                .filter(|value| value.is_finite() && (self != Self::StepSize || *value > 0.0))
                .map(|value| FocuserValue::Number { value }),
        };
        decoded.ok_or_else(bad_reading)
    }
}
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum FocuserValue {
    Boolean { value: bool },
    Integer { value: i32 },
    Number { value: f64 },
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserSample {
    pub value: FocuserValue,
    pub age_seconds: f64,
    pub source: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub revision: Uuid,
}
pub(crate) fn cached_property(
    state: &crate::source::SourceSnapshot,
    property: FocuserProperty,
    now: Duration,
) -> Result<FocuserSample, SourceError> {
    if !state.transport_connected {
        return Err(disconnected());
    }
    if let Some(error) = &state.error {
        return Err(error.clone());
    }
    let key = property.member();
    if property == FocuserProperty::Position
        && state.values.get("absolute").and_then(Value::as_bool) == Some(false)
    {
        return Err(SourceError::new(
            ErrorKind::Unsupported,
            "Relative focusers do not report absolute position",
        ));
    }
    if let Some(error) = state.sample_errors.get(key) {
        return Err(error.clone());
    }
    let value = property.decode(
        state
            .values
            .get(key)
            .ok_or_else(|| unavailable("No focuser sample has been received"))?,
    )?;
    let sampled = state
        .sample_started_seconds
        .get(key)
        .copied()
        .or(state.sampled_at_seconds)
        .ok_or_else(|| unavailable("No focuser sample has been received"))?;
    let elapsed = now.as_secs_f64() - sampled;
    let upstream_age = state.sample_ages_seconds.get(key).copied().unwrap_or(0.0);
    let age_seconds = elapsed + upstream_age;
    if elapsed < 0.0 || upstream_age < 0.0 || !age_seconds.is_finite() {
        return Err(bad_reading());
    }
    Ok(FocuserSample {
        value,
        age_seconds,
        source: state.source,
        generation: state.generation,
        sequence: state
            .sample_sequences
            .get(key)
            .copied()
            .unwrap_or(state.sequence),
        revision: state.revision,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocuserCapabilities {
    pub absolute: bool,
    pub max_step: i32,
    pub max_increment: i32,
    pub temp_comp_available: bool,
}

pub struct FocuserController {
    source: Arc<SourceHandle>,
    connection_timeout: Duration,
}
impl FocuserController {
    pub(crate) fn source(&self) -> &Arc<SourceHandle> {
        &self.source
    }
    /// Construction performs no I/O. The runtime supplies the source's configured
    /// connection deadline after validating its device class.
    pub fn new(
        source: Arc<SourceHandle>,
        connection_timeout: Duration,
    ) -> Result<Self, SourceError> {
        if connection_timeout.is_zero() || connection_timeout > Duration::from_secs(300) {
            return Err(invalid("Invalid focuser connection deadline"));
        }
        Ok(Self {
            source,
            connection_timeout,
        })
    }

    pub async fn connect(&self) -> Result<FocuserSession, SourceError> {
        let connect = async {
            let session = FocuserSession {
                source: TypedSourceSession::connect(self.source.clone()).await?,
            };
            session.capabilities().await?;
            Ok(session)
        };
        tokio::time::timeout(self.connection_timeout, connect)
            .await
            .map_err(|_| {
                SourceError::new(
                    ErrorKind::Unavailable,
                    "Focuser connection deadline expired",
                )
            })?
    }
}

/// Each connected output owns only its own source lease. Dropping it releases
/// that lease without halting motion or disconnecting other clients.
pub struct FocuserSession {
    source: TypedSourceSession,
}
impl FocuserSession {
    pub async fn property(&self, property: FocuserProperty) -> Result<Value, SourceError> {
        if property == FocuserProperty::Position {
            return Ok(json!(self.position().await?));
        }
        let value = self.read(property.member()).await?;
        property.decode(&value)?;
        Ok(value)
    }
    pub fn connected(&self) -> bool {
        self.source.connected()
    }
    pub(crate) fn cached_sample(
        &self,
        property: FocuserProperty,
        now: Duration,
    ) -> Result<FocuserSample, SourceError> {
        cached_property(&self.source.snapshot()?, property, now)
    }
    pub(crate) fn device_state(&self, now: Duration) -> Values {
        let Ok(state) = self.source.snapshot() else {
            return Values::new();
        };
        [
            (FocuserProperty::IsMoving, "IsMoving"),
            (FocuserProperty::Position, "Position"),
            (FocuserProperty::Temperature, "Temperature"),
        ]
        .into_iter()
        .filter_map(|(property, name)| {
            cached_property(&state, property, now).ok().map(|sample| {
                let value = match sample.value {
                    FocuserValue::Boolean { value } => json!(value),
                    FocuserValue::Integer { value } => json!(value),
                    FocuserValue::Number { value } => json!(value),
                };
                (name.into(), value)
            })
        })
        .collect()
    }
    pub fn generation(&self) -> Uuid {
        self.source.generation()
    }
    async fn read(&self, member: &str) -> Result<Value, SourceError> {
        self.source.read(member).await
    }
    async fn boolean(&self, member: &str) -> Result<bool, SourceError> {
        self.read(member).await?.as_bool().ok_or_else(bad_reading)
    }
    async fn integer(&self, member: &str) -> Result<i32, SourceError> {
        self.read(member)
            .await?
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(bad_reading)
    }
    async fn number(&self, member: &str) -> Result<f64, SourceError> {
        self.read(member)
            .await?
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(bad_reading)
    }

    /// Read current limits, rather than trusting limits cached at connection.
    /// The source fence covers every read and the later mutation.
    pub async fn capabilities(&self) -> Result<FocuserCapabilities, SourceError> {
        let capabilities = FocuserCapabilities {
            absolute: self.boolean("absolute").await?,
            max_step: self.integer("maxstep").await?,
            max_increment: self.integer("maxincrement").await?,
            temp_comp_available: self.boolean("tempcompavailable").await?,
        };
        if capabilities.max_step <= 0 || capabilities.max_increment <= 0 {
            return Err(bad_reading());
        }
        Ok(capabilities)
    }
    pub async fn position(&self) -> Result<i32, SourceError> {
        let capabilities = self.capabilities().await?;
        if !capabilities.absolute {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Relative focusers do not report absolute position",
            ));
        }
        let position = self.integer("position").await?;
        if !(0..=capabilities.max_step).contains(&position) {
            return Err(bad_reading());
        }
        Ok(position)
    }
    pub async fn is_moving(&self) -> Result<bool, SourceError> {
        self.boolean("ismoving").await
    }
    pub async fn temp_comp(&self) -> Result<bool, SourceError> {
        self.boolean("tempcomp").await
    }
    pub async fn temperature(&self) -> Result<f64, SourceError> {
        self.number("temperature").await
    }
    pub async fn step_size(&self) -> Result<f64, SourceError> {
        let value = self.number("stepsize").await?;
        if value <= 0.0 {
            return Err(bad_reading());
        }
        Ok(value)
    }

    /// Acknowledges motion start; callers observe IsMoving for completion. It
    /// never disables temperature compensation or retries a dispatched command.
    pub async fn move_to(&self, position: i32) -> Result<(), SourceError> {
        let operation = self.source.operation().await?;
        let capabilities = self.capabilities().await?;
        let valid = if capabilities.absolute {
            (0..=capabilities.max_step).contains(&position)
        } else {
            position.unsigned_abs() <= capabilities.max_increment as u32
        };
        if !valid {
            return Err(invalid("Focuser movement is outside the source limits"));
        }
        if self.is_moving().await? {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "Focuser is already moving",
            ));
        }
        if capabilities.absolute {
            let current = self.integer("position").await?;
            if !(0..=capabilities.max_step).contains(&current) {
                return Err(bad_reading());
            }
            if current.abs_diff(position) > capabilities.max_increment as u32 {
                return Err(invalid(
                    "Focuser movement exceeds the source maximum increment",
                ));
            }
        }
        self.source
            .write(
                &operation,
                "move",
                Values::from([("Position".into(), json!(position))]),
            )
            .await
    }
    /// Optional support is determined by the source; setup never probes Halt by
    /// actuating it. A halt failure is not replaced by a guessed motion command.
    pub async fn halt(&self) -> Result<(), SourceError> {
        let operation = self.source.operation().await?;
        self.source.write(&operation, "halt", Values::new()).await
    }
    pub async fn set_temp_comp(&self, enabled: bool) -> Result<(), SourceError> {
        let operation = self.source.operation().await?;
        if !self.capabilities().await?.temp_comp_available {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Temperature compensation is not supported",
            ));
        }
        self.source
            .write(
                &operation,
                "tempcomp",
                Values::from([("TempComp".into(), json!(enabled))]),
            )
            .await
    }
}
fn bad_reading() -> SourceError {
    unavailable("Source returned an invalid focuser property")
}
fn disconnected() -> SourceError {
    SourceError::new(
        ErrorKind::Disconnected,
        "Focuser session changed; reconnect explicitly",
    )
}
