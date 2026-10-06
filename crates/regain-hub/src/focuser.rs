//! Typed focuser operations over the shared source actor. A session is bound to
//! one transport generation; reconnect explicitly to adopt a replacement device.
use crate::{
    readout::{SourceLease, invalid, unavailable},
    source::{ErrorKind, SourceError, SourceHandle, Values},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

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
            // Acquire can acknowledge a pending asynchronous connection. Observe
            // actual readiness before fencing the first capability request.
            let lease = SourceLease::acquire(self.source.clone()).await?;
            let mut status = self.source.status();
            let generation = loop {
                let state = status.borrow_and_update().clone();
                if state.transport_connected {
                    break state.generation;
                }
                if let Some(error) = state.error
                    && matches!(error.kind, ErrorKind::Permanent | ErrorKind::Unsupported)
                {
                    return Err(error);
                }
                status.changed().await.map_err(|_| disconnected())?;
            };
            let session = FocuserSession { lease, generation };
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
    lease: SourceLease,
    generation: Uuid,
}
impl FocuserSession {
    pub fn generation(&self) -> Uuid {
        self.generation
    }

    fn check_generation(&self) -> Result<(), SourceError> {
        let state = self.lease.source.snapshot();
        if state.generation != self.generation || !state.transport_connected {
            return Err(disconnected());
        }
        Ok(())
    }

    async fn read(&self, member: &str) -> Result<Value, SourceError> {
        self.check_generation()?;
        let value = self
            .lease
            .source
            .read_fenced(self.lease.id, member, Values::new(), Some(self.generation))
            .await?;
        self.check_generation()?;
        Ok(value)
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

    async fn operation(&self) -> Result<SourceLease, SourceError> {
        if self.lease.source.snapshot().write_uncertain {
            return Err(SourceError::uncertain());
        }
        self.check_generation()?;
        // Unique control ownership includes overlapping calls from this session.
        // Cancellation drops the guard and queues release in source FIFO order.
        let operation = SourceLease::acquire(self.lease.source.clone()).await?;
        self.check_generation()?;
        operation.source.control(operation.id, true).await?;
        self.check_generation()?;
        Ok(operation)
    }
    async fn write(
        &self,
        operation: &SourceLease,
        member: &str,
        parameters: Values,
    ) -> Result<(), SourceError> {
        self.check_generation()?;
        operation
            .source
            .write_fenced(operation.id, member, parameters, Some(self.generation))
            .await?;
        Ok(())
    }

    /// Acknowledges motion start; callers observe IsMoving for completion. It
    /// never disables temperature compensation or retries a dispatched command.
    pub async fn move_to(&self, position: i32) -> Result<(), SourceError> {
        let operation = self.operation().await?;
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
        self.write(
            &operation,
            "move",
            Values::from([("Position".into(), json!(position))]),
        )
        .await
    }
    /// Optional support is determined by the source; setup never probes Halt by
    /// actuating it. A halt failure is not replaced by a guessed motion command.
    pub async fn halt(&self) -> Result<(), SourceError> {
        let operation = self.operation().await?;
        self.write(&operation, "halt", Values::new()).await
    }
    pub async fn set_temp_comp(&self, enabled: bool) -> Result<(), SourceError> {
        let operation = self.operation().await?;
        if !self.capabilities().await?.temp_comp_available {
            return Err(SourceError::new(
                ErrorKind::Unsupported,
                "Temperature compensation is not supported",
            ));
        }
        self.write(
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
