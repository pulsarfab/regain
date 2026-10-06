//! Connection, generation and command admission shared by typed accessories.
//! Device-specific capabilities, ranges and semantics stay in their controllers.
use crate::{
    readout::SourceLease,
    source::{ErrorKind, SourceError, SourceHandle, SourceSnapshot, Values},
};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

pub(crate) struct TypedSourceSession {
    lease: SourceLease,
    generation: Uuid,
}
impl TypedSourceSession {
    /// The controller bounds the whole handshake, including its initial
    /// capability reads. A pending connection acknowledgement is not readiness.
    pub(crate) async fn connect(source: Arc<SourceHandle>) -> Result<Self, SourceError> {
        let lease = SourceLease::acquire(source.clone()).await?;
        let mut status = source.status();
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
        Ok(Self { lease, generation })
    }
    pub(crate) fn generation(&self) -> Uuid {
        self.generation
    }
    pub(crate) fn snapshot(&self) -> Result<SourceSnapshot, SourceError> {
        let state = self.lease.source.snapshot();
        if state.generation != self.generation || !state.transport_connected {
            return Err(disconnected());
        }
        Ok(state)
    }
    pub(crate) fn connected(&self) -> bool {
        self.snapshot().is_ok()
    }
    pub(crate) async fn read(&self, member: &str) -> Result<Value, SourceError> {
        self.snapshot()?;
        let value = self
            .lease
            .source
            .read_fenced(self.lease.id, member, Values::new(), Some(self.generation))
            .await?;
        self.snapshot()?;
        Ok(value)
    }
    pub(crate) async fn operation(&self) -> Result<SourceLease, SourceError> {
        if self.lease.source.snapshot().write_uncertain {
            return Err(SourceError::uncertain());
        }
        self.snapshot()?;
        // Unique control ownership also arbitrates simultaneous calls made by
        // one session. Dropping the guard queues release in source FIFO order.
        let operation = SourceLease::acquire(self.lease.source.clone()).await?;
        self.snapshot()?;
        operation.source.control(operation.id, true).await?;
        self.snapshot()?;
        Ok(operation)
    }
    pub(crate) async fn write(
        &self,
        operation: &SourceLease,
        member: &str,
        parameters: Values,
    ) -> Result<(), SourceError> {
        self.snapshot()?;
        operation
            .source
            .write_fenced(operation.id, member, parameters, Some(self.generation))
            .await?;
        Ok(())
    }
}
fn disconnected() -> SourceError {
    SourceError::new(
        ErrorKind::Disconnected,
        "Typed source session changed; reconnect explicitly",
    )
}
