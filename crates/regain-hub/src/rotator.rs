//! Typed rotator semantics over shared actors. Source drivers retain their angle
//! transforms, cable management and persistent reference; the hub does not invent
//! an offset or normalize an invalid upstream reading into a plausible position.
use crate::{
    readout::invalid,
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
pub enum RotatorProperty {
    CanReverse,
    IsMoving,
    MechanicalPosition,
    Position,
    Reverse,
    StepSize,
    TargetPosition,
}
impl RotatorProperty {
    pub const ALL: [Self; 7] = [
        Self::CanReverse,
        Self::IsMoving,
        Self::MechanicalPosition,
        Self::Position,
        Self::Reverse,
        Self::StepSize,
        Self::TargetPosition,
    ];
    pub fn member(self) -> &'static str {
        match self {
            Self::CanReverse => "canreverse",
            Self::IsMoving => "ismoving",
            Self::MechanicalPosition => "mechanicalposition",
            Self::Position => "position",
            Self::Reverse => "reverse",
            Self::StepSize => "stepsize",
            Self::TargetPosition => "targetposition",
        }
    }
    pub fn value_type(self) -> &'static str {
        match self {
            Self::CanReverse | Self::IsMoving | Self::Reverse => "boolean",
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
    pub fn decode(self, value: &Value) -> Result<RotatorValue, SourceError> {
        match self.value_type() {
            "boolean" => value.as_bool().map(|value| RotatorValue::Boolean { value }),
            _ => value
                .as_f64()
                .filter(|value| {
                    finite_single(*value)
                        && if self == Self::StepSize {
                            *value > 0.0 && (*value as f32) > 0.0
                        } else {
                            (0.0..360.0).contains(value) && (*value as f32) < 360.0
                        }
                })
                .map(|value| RotatorValue::Number { value }),
        }
        .ok_or_else(|| {
            SourceError::new(
                ErrorKind::Unavailable,
                "Source returned an invalid rotator property",
            )
        })
    }
}
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum RotatorValue {
    Boolean { value: bool },
    Number { value: f64 },
}

pub type RotatorSample = crate::readout::TypedSample<RotatorValue>;
pub(crate) fn cached_property(
    state: &crate::source::SourceSnapshot,
    property: RotatorProperty,
    now: Duration,
) -> Result<RotatorSample, SourceError> {
    if !state.transport_connected {
        return Err(SourceError::new(
            ErrorKind::Disconnected,
            "Source is disconnected",
        ));
    }
    if let Some(error) = &state.error {
        return Err(error.clone());
    }
    let key = property.member();
    if let Some(error) = state.sample_errors.get(key) {
        return Err(error.clone());
    }
    let unavailable = || {
        SourceError::new(
            ErrorKind::Unavailable,
            "No rotator sample has been received",
        )
    };
    let value = property.decode(state.values.get(key).ok_or_else(unavailable)?)?;
    crate::readout::typed_sample(state, key, now, value)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotatorCapabilities {
    pub can_reverse: bool,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RotatorMotionReceipt {
    pub expected_target: f64,
    pub target_position: f64,
}

pub struct RotatorController {
    source: Arc<SourceHandle>,
    connection_timeout: Duration,
}
impl RotatorController {
    pub(crate) fn source(&self) -> &Arc<SourceHandle> {
        &self.source
    }
    /// No device I/O or mutation at construction. Bound the whole handshake,
    /// including initial capability reads, using the configured source deadline.
    pub fn new(
        source: Arc<SourceHandle>,
        connection_timeout: Duration,
    ) -> Result<Self, SourceError> {
        if connection_timeout.is_zero() || connection_timeout > Duration::from_secs(300) {
            return Err(invalid("Invalid rotator connection deadline"));
        }
        Ok(Self {
            source,
            connection_timeout,
        })
    }
    pub async fn connect(&self) -> Result<RotatorSession, SourceError> {
        self.connect_required(false).await
    }
    pub(crate) async fn connect_modern(&self) -> Result<RotatorSession, SourceError> {
        self.connect_required(true).await
    }
    async fn connect_required(&self, modern: bool) -> Result<RotatorSession, SourceError> {
        tokio::time::timeout(self.connection_timeout, async {
            let session = RotatorSession {
                source: TypedSourceSession::connect(self.source.clone()).await?,
            };
            let capabilities = session.capabilities().await?;
            if modern {
                if !capabilities.can_reverse {
                    return Err(required_reversal());
                }
                session
                    .property(RotatorProperty::Reverse)
                    .await
                    .map_err(|error| {
                        if error.kind == ErrorKind::Unsupported {
                            SourceError {
                                upstream_code: error.upstream_code,
                                ..required_reversal()
                            }
                        } else {
                            error
                        }
                    })?;
            }
            Ok(session)
        })
        .await
        .map_err(|_| {
            SourceError::new(
                ErrorKind::Unavailable,
                "Rotator connection deadline expired",
            )
        })?
    }
}
pub struct RotatorSession {
    source: TypedSourceSession,
}
impl RotatorSession {
    pub(crate) fn cached_sample(
        &self,
        property: RotatorProperty,
    ) -> Result<RotatorSample, SourceError> {
        let (state, now) = self.source.timed_snapshot()?;
        cached_property(&state, property, now)
    }
    pub(crate) fn device_state(&self) -> Values {
        let Ok((state, now)) = self.source.timed_snapshot() else {
            return Values::new();
        };
        [
            (RotatorProperty::IsMoving, "IsMoving"),
            (RotatorProperty::MechanicalPosition, "MechanicalPosition"),
            (RotatorProperty::Position, "Position"),
        ]
        .into_iter()
        .filter_map(|(property, name)| {
            cached_property(&state, property, now).ok().map(|sample| {
                let value = match sample.value {
                    RotatorValue::Boolean { value } => json!(value),
                    RotatorValue::Number { value } => json!(value),
                };
                (name.into(), value)
            })
        })
        .collect()
    }
    pub fn connected(&self) -> bool {
        self.source.connected()
    }
    pub fn generation(&self) -> Uuid {
        self.source.generation()
    }
    pub async fn property(&self, property: RotatorProperty) -> Result<Value, SourceError> {
        let value = self.source.read(property.member()).await?;
        property.decode(&value)?;
        Ok(value)
    }
    pub async fn capabilities(&self) -> Result<RotatorCapabilities, SourceError> {
        Ok(RotatorCapabilities {
            can_reverse: self
                .property(RotatorProperty::CanReverse)
                .await?
                .as_bool()
                .unwrap(),
        })
    }
    pub async fn is_moving(&self) -> Result<bool, SourceError> {
        Ok(self
            .property(RotatorProperty::IsMoving)
            .await?
            .as_bool()
            .unwrap())
    }
    async fn idle(&self) -> Result<(), SourceError> {
        if self.is_moving().await? {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "Rotator is already moving",
            ));
        }
        Ok(())
    }
    async fn position_command(
        &self,
        member: &str,
        degrees: f64,
        absolute: bool,
        report_target: bool,
    ) -> Result<Option<RotatorMotionReceipt>, SourceError> {
        if !finite_single(degrees)
            || absolute && (!(0.0..360.0).contains(&degrees) || (degrees as f32) >= 360.0)
        {
            return Err(invalid("Invalid rotator angle"));
        }
        let operation = self.source.operation().await?;
        self.idle().await?;
        let expected_target = if report_target {
            let position = self
                .property(RotatorProperty::Position)
                .await?
                .as_f64()
                .unwrap();
            // Reduce before adding so a large Single distance cannot erase
            // the starting angle's low bits. Preserve its signed wire value.
            Some((position + degrees.rem_euclid(360.0)).rem_euclid(360.0))
        } else {
            None
        };
        self.source
            .write(
                &operation,
                member,
                Values::from([("Position".into(), json!(degrees))]),
            )
            .await?;
        if report_target {
            // Read while holding the same exclusive command lease: a sibling
            // cannot replace the accepted relative target between ACK and read.
            let target = self.property(RotatorProperty::TargetPosition).await.map_err(|_| {
                SourceError::new(ErrorKind::Unavailable, "Rotator move was accepted but its target could not be verified; do not replay")
            })?;
            Ok(Some(RotatorMotionReceipt {
                expected_target: expected_target.unwrap(),
                target_position: target.as_f64().unwrap(),
            }))
        } else {
            Ok(None)
        }
    }
    /// Preserve the requested signed relative angle. The source handles wrapping
    /// and any hardware limit; never turn it into a guessed absolute target.
    pub async fn move_relative(&self, degrees: f64) -> Result<(), SourceError> {
        self.position_command("move", degrees, false, false)
            .await
            .map(|_| ())
    }
    pub(crate) async fn move_relative_target(
        &self,
        degrees: f64,
    ) -> Result<RotatorMotionReceipt, SourceError> {
        Ok(self
            .position_command("move", degrees, false, true)
            .await?
            .unwrap())
    }
    pub async fn move_absolute(&self, degrees: f64) -> Result<(), SourceError> {
        self.position_command("moveabsolute", degrees, true, false)
            .await
            .map(|_| ())
    }
    pub async fn move_mechanical(&self, degrees: f64) -> Result<(), SourceError> {
        self.position_command("movemechanical", degrees, true, false)
            .await
            .map(|_| ())
    }
    /// Dispatch the upstream reference operation without synthesizing motion.
    /// Persistent offset storage belongs to the source adapter/driver.
    pub async fn sync(&self, degrees: f64) -> Result<(), SourceError> {
        self.position_command("sync", degrees, true, false)
            .await
            .map(|_| ())
    }
    pub async fn set_reverse(&self, enabled: bool) -> Result<(), SourceError> {
        let operation = self.source.operation().await?;
        if !self.capabilities().await?.can_reverse {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Source does not support reversal",
            ));
        }
        self.idle().await?;
        self.source
            .write(
                &operation,
                "reverse",
                Values::from([("Reverse".into(), json!(enabled))]),
            )
            .await
    }
    /// Optional upstream Halt errors remain errors. Neither disconnect nor
    /// cancellation invokes this command automatically.
    pub async fn halt(&self) -> Result<(), SourceError> {
        let operation = self.source.operation().await?;
        self.source.write(&operation, "halt", Values::new()).await
    }
}
fn finite_single(value: f64) -> bool {
    value.is_finite() && value.abs() <= f32::MAX as f64
}
pub(crate) fn required_reversal() -> SourceError {
    SourceError::new(
        ErrorKind::Unavailable,
        "Source cannot provide required rotator reversal",
    )
}
