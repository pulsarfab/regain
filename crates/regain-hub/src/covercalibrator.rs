//! Cover and illumination commands over the shared typed source session. An
//! accepted command is not completion; disconnect never closes or darkens a panel.
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
pub enum CoverCalibratorProperty {
    Brightness,
    MaxBrightness,
    CoverState,
    CalibratorState,
    CoverMoving,
    CalibratorChanging,
}
impl CoverCalibratorProperty {
    pub const ALL: [Self; 6] = [
        Self::Brightness,
        Self::MaxBrightness,
        Self::CoverState,
        Self::CalibratorState,
        Self::CoverMoving,
        Self::CalibratorChanging,
    ];
    pub fn member(self) -> &'static str {
        match self {
            Self::Brightness => "brightness",
            Self::MaxBrightness => "maxbrightness",
            Self::CoverState => "coverstate",
            Self::CalibratorState => "calibratorstate",
            Self::CoverMoving => "covermoving",
            Self::CalibratorChanging => "calibratorchanging",
        }
    }
    pub fn value_type(self) -> &'static str {
        match self {
            Self::CoverMoving | Self::CalibratorChanging => "boolean",
            _ => "integer",
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
    pub fn decode(self, value: &Value) -> Result<CoverCalibratorValue, SourceError> {
        if self.value_type() == "boolean" {
            return value
                .as_bool()
                .map(|value| CoverCalibratorValue::Boolean { value })
                .ok_or_else(bad_reading);
        }
        value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .filter(|value| match self {
                Self::CoverState | Self::CalibratorState => (0..=5).contains(value),
                Self::MaxBrightness => *value > 0,
                _ => *value >= 0,
            })
            .map(|value| CoverCalibratorValue::Integer { value })
            .ok_or_else(bad_reading)
    }
}
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum CoverCalibratorValue {
    Integer { value: i32 },
    Boolean { value: bool },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoverCalibratorCapabilities {
    pub cover_present: bool,
    pub calibrator_present: bool,
    pub max_brightness: Option<i32>,
}
pub struct CoverCalibratorController {
    source: Arc<SourceHandle>,
    connection_timeout: Duration,
}
impl CoverCalibratorController {
    /// Construction is inert. The timeout bounds source admission and all initial
    /// property reads, including pending asynchronous upstream connections.
    pub fn new(
        source: Arc<SourceHandle>,
        connection_timeout: Duration,
    ) -> Result<Self, SourceError> {
        if connection_timeout.is_zero() || connection_timeout > Duration::from_secs(300) {
            return Err(invalid("Invalid panel connection deadline"));
        }
        Ok(Self {
            source,
            connection_timeout,
        })
    }
    pub async fn connect(&self) -> Result<CoverCalibratorSession, SourceError> {
        tokio::time::timeout(self.connection_timeout, async {
            let session = CoverCalibratorSession {
                source: TypedSourceSession::connect(self.source.clone()).await?,
            };
            let capabilities = session.capabilities().await?;
            if capabilities.calibrator_present {
                session
                    .property(CoverCalibratorProperty::Brightness)
                    .await?;
            }
            if !session.legacy()? {
                session
                    .property(CoverCalibratorProperty::CoverMoving)
                    .await?;
                session
                    .property(CoverCalibratorProperty::CalibratorChanging)
                    .await?;
            }
            Ok(session)
        })
        .await
        .map_err(|_| {
            SourceError::new(ErrorKind::Unavailable, "Panel connection deadline expired")
        })?
    }
}
pub struct CoverCalibratorSession {
    source: TypedSourceSession,
}
impl CoverCalibratorSession {
    pub fn connected(&self) -> bool {
        self.source.connected()
    }
    pub fn generation(&self) -> Uuid {
        self.source.generation()
    }
    fn legacy(&self) -> Result<bool, SourceError> {
        Ok(self
            .source
            .snapshot()?
            .connection_info
            .and_then(|info| info.interface_version)
            == Some(1))
    }
    async fn integer(&self, property: CoverCalibratorProperty) -> Result<i32, SourceError> {
        let value = self.source.read(property.member()).await?;
        match property.decode(&value)? {
            CoverCalibratorValue::Integer { value } => Ok(value),
            _ => unreachable!(),
        }
    }
    pub async fn capabilities(&self) -> Result<CoverCalibratorCapabilities, SourceError> {
        use CoverCalibratorProperty::*;
        let cover_present = self.integer(CoverState).await? != 0;
        let calibrator_present = self.integer(CalibratorState).await? != 0;
        let max_brightness = if calibrator_present {
            Some(self.integer(MaxBrightness).await?)
        } else {
            None
        };
        Ok(CoverCalibratorCapabilities {
            cover_present,
            calibrator_present,
            max_brightness,
        })
    }
    async fn light(&self) -> Result<(i32, i32), SourceError> {
        use CoverCalibratorProperty::*;
        let state = self.integer(CalibratorState).await?;
        if state == 0 {
            return Err(unsupported());
        }
        Ok((state, self.integer(MaxBrightness).await?))
    }
    pub async fn property(&self, property: CoverCalibratorProperty) -> Result<Value, SourceError> {
        use CoverCalibratorProperty::*;
        match property {
            CoverState | CalibratorState => Ok(json!(self.integer(property).await?)),
            MaxBrightness => Ok(json!(self.light().await?.1)),
            Brightness => {
                let (state, maximum) = self.light().await?;
                let brightness = self.integer(Brightness).await?;
                if brightness > maximum || state == 1 && brightness != 0 {
                    return Err(bad_reading());
                }
                Ok(json!(brightness))
            }
            CoverMoving | CalibratorChanging => {
                // Only a negotiated V1 interface lacks these properties. Do not
                // hide a malformed or missing mandatory modern property.
                if self.legacy()? {
                    let state = self
                        .integer(if property == CoverMoving {
                            CoverState
                        } else {
                            CalibratorState
                        })
                        .await?;
                    return match state {
                        0 | 1 | 3 => Ok(json!(false)),
                        2 => Ok(json!(true)),
                        // Unknown/Error does not establish motion completion.
                        _ => Err(bad_reading()),
                    };
                }
                let value = self.source.read(property.member()).await?;
                property.decode(&value)?;
                Ok(value)
            }
        }
    }
    async fn cover_command(&self, member: &str) -> Result<(), SourceError> {
        let operation = self.source.operation().await?;
        if self.integer(CoverCalibratorProperty::CoverState).await? == 0 {
            return Err(unsupported());
        }
        self.source.write(&operation, member, Values::new()).await
    }
    pub async fn open_cover(&self) -> Result<(), SourceError> {
        self.cover_command("opencover").await
    }
    pub async fn close_cover(&self) -> Result<(), SourceError> {
        self.cover_command("closecover").await
    }
    pub async fn halt_cover(&self) -> Result<(), SourceError> {
        self.cover_command("haltcover").await
    }
    pub async fn calibrator_on(&self, brightness: i32) -> Result<(), SourceError> {
        if brightness < 0 {
            return Err(invalid("Brightness must be nonnegative"));
        }
        let operation = self.source.operation().await?;
        let (_, maximum) = self.light().await?;
        if brightness > maximum {
            return Err(invalid("Brightness exceeds the source maximum"));
        }
        self.source
            .write(
                &operation,
                "calibratoron",
                Values::from([("Brightness".into(), json!(brightness))]),
            )
            .await
    }
    pub async fn calibrator_off(&self) -> Result<(), SourceError> {
        let operation = self.source.operation().await?;
        if self
            .integer(CoverCalibratorProperty::CalibratorState)
            .await?
            == 0
        {
            return Err(unsupported());
        }
        self.source
            .write(&operation, "calibratoroff", Values::new())
            .await
    }
}
fn bad_reading() -> SourceError {
    SourceError::new(
        ErrorKind::Unavailable,
        "Source returned an invalid or unavailable panel property",
    )
}
fn unsupported() -> SourceError {
    SourceError::new(
        ErrorKind::Unsupported,
        "This panel capability is not present",
    )
}
