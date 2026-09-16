//! Transport failures and time bounds shared by all native USB implementations.
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Failure(pub Value);
impl Failure {
    pub fn new(category: &str, expected: usize, received: usize, expired: bool) -> Self {
        Self(json!({"category":category,"expectedBytes":expected,
            "receivedBytes":received,"deadlineExpired":expired}))
    }
    pub fn details(error: &anyhow::Error) -> Value {
        error
            .downcast_ref::<Self>()
            .map_or(Value::Null, |e| e.0.clone())
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "USB transfer failed: {}", self.0)
    }
}
impl std::error::Error for Failure {}

pub struct Budget {
    started: Instant,
    duration: Duration,
}
impl Budget {
    pub fn new(duration: Duration) -> Self {
        Self {
            started: Instant::now(),
            duration,
        }
    }
    pub fn timeout_ms(&self, request_ms: u32) -> Option<u32> {
        remaining_ms(self.duration, self.started.elapsed(), request_ms)
    }
}
fn remaining_ms(duration: Duration, elapsed: Duration, request_ms: u32) -> Option<u32> {
    let remaining = duration.checked_sub(elapsed)?;
    if remaining.is_zero() {
        return None;
    }
    Some(remaining.as_millis().max(1).min(u128::from(request_ms)) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_is_total_not_extended_by_each_chunk() {
        let total = Duration::from_secs(6);
        assert_eq!(remaining_ms(total, Duration::ZERO, 5000), Some(5000));
        assert_eq!(
            remaining_ms(total, Duration::from_millis(5800), 5000),
            Some(200)
        );
        assert_eq!(remaining_ms(total, total, 5000), None);
        assert_eq!(
            remaining_ms(total, total + Duration::from_secs(1), 5000),
            None
        );
    }
    #[test]
    fn structured_failure_survives_error_context() {
        let error = anyhow::Error::new(Failure::new("short_read", 1024, 512, false))
            .context("bulk chunk 12")
            .context("camera capture");
        assert_eq!(Failure::details(&error)["receivedBytes"], 512);
        assert_eq!(Failure::details(&error)["category"], "short_read");
    }
}
