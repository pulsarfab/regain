//! Shared scalar sample interpretation and cancellation-safe source leases.
use crate::{
    config::Readout,
    source::{ErrorKind, SourceError, SourceHandle, SourceSnapshot},
};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

impl Readout {
    pub fn sample_key(&self) -> String {
        match self {
            Self::Property { property, .. } => property.clone(),
            Self::Channel { channel, .. } => format!("channel/{channel}"),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScalarSample {
    pub value: f64,
    pub age_seconds: f64,
    pub source: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub revision: Uuid,
}
pub fn scalar(
    state: &SourceSnapshot,
    readout: &Readout,
    now: Duration,
) -> Result<ScalarSample, SourceError> {
    if state.source != readout.source() {
        return Err(invalid(
            "Sample source does not match the configured readout",
        ));
    }
    if !state.transport_connected {
        return Err(SourceError::new(
            ErrorKind::Disconnected,
            "Source is disconnected",
        ));
    }
    if let Some(error) = &state.error {
        return Err(error.clone());
    }
    let key = readout.sample_key();
    if let Some(error) = state.sample_errors.get(&key) {
        return Err(error.clone());
    }
    let value = state
        .values
        .get(&key)
        .and_then(|value| {
            value.as_f64().or_else(|| {
                if matches!(readout,Readout::Property {property,..} if property=="issafe") {
                    value.as_bool().map(|value| if value { 1.0 } else { 0.0 })
                } else {
                    None
                }
            })
        })
        .filter(|value| value.is_finite())
        .ok_or_else(|| unavailable("No valid scalar reading"))?;
    let sampled = state
        .sampled_at_seconds
        .ok_or_else(|| unavailable("No sample has been received"))?;
    let upstream_age = state.sample_ages_seconds.get(&key).copied().unwrap_or(0.0);
    let elapsed = now.as_secs_f64() - sampled;
    if !elapsed.is_finite() || elapsed < 0.0 || !upstream_age.is_finite() || upstream_age < 0.0 {
        return Err(unavailable("Sample clock or sensor age is invalid"));
    }
    let age_seconds = elapsed + upstream_age;
    if !age_seconds.is_finite() {
        return Err(unavailable("Sample age is invalid"));
    }
    Ok(ScalarSample {
        value,
        age_seconds,
        source: state.source,
        generation: state.generation,
        sequence: state.sequence,
        revision: state.revision,
    })
}
pub fn invalid(message: &'static str) -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, message)
}
pub fn unavailable(message: &'static str) -> SourceError {
    SourceError::new(ErrorKind::Unavailable, message)
}

/// IPC/output sessions retain this guard. Cancellation queues release even if
/// acquire had not yet answered; source command ordering makes cleanup safe.
pub struct SourceLease {
    pub source: Arc<SourceHandle>,
    pub id: Uuid,
    runtime: tokio::runtime::Handle,
}
impl SourceLease {
    pub async fn acquire(source: Arc<SourceHandle>) -> Result<Self, SourceError> {
        let lease = Self {
            source,
            id: Uuid::new_v4(),
            runtime: tokio::runtime::Handle::current(),
        };
        lease.source.acquire(lease.id).await?;
        Ok(lease)
    }
}
impl Drop for SourceLease {
    fn drop(&mut self) {
        let source = self.source.clone();
        let id = self.id;
        self.runtime.spawn(async move {
            let _ = source.release(id).await;
        });
    }
}
