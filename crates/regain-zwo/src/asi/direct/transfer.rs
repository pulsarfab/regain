//! Transport failures and time bounds shared by all native USB implementations.
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

/// Evidence from successfully completed chunks of one frozen DDR frame.
/// Envelope counters may change on replay; pixel bytes may not.
#[derive(Default)]
pub struct Continuity {
    chunks: Vec<(usize, [u8; 32])>,
    verified_bytes: usize,
}
#[derive(Debug)]
pub struct ChangedFrame;
impl std::fmt::Display for ChangedFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("retained frame pixels changed during recovery")
    }
}
impl std::error::Error for ChangedFrame {}
impl Continuity {
    pub fn has_pixels(&self) -> bool {
        !self.chunks.is_empty()
    }
    pub fn verified_bytes(&self) -> usize {
        self.verified_bytes
    }
    pub fn observe(
        &mut self,
        number: usize,
        offset: usize,
        frame_bytes: usize,
        data: &[u8],
    ) -> Result<()> {
        ensure!(
            data.len() >= 8 && offset + data.len() <= frame_bytes,
            "invalid continuity chunk"
        );
        let start = if offset == 0 { 4 } else { 0 };
        let end = data.len()
            - if offset + data.len() == frame_bytes {
                4
            } else {
                0
            };
        let digest: [u8; 32] = Sha256::digest(&data[start..end]).into();
        let proof = (data.len(), digest);
        if let Some(previous) = self.chunks.get(number) {
            ensure!(*previous == proof, ChangedFrame);
            self.verified_bytes += end - start;
        } else {
            ensure!(
                number == self.chunks.len(),
                "nonsequential continuity chunk"
            );
            self.chunks.push(proof);
        }
        Ok(())
    }
}

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
    fn reread_must_match_all_known_pixels_but_envelope_can_change() {
        let mut proof = Continuity::default();
        let a = vec![17; 1024];
        let mut b = a.clone();
        proof.observe(0, 0, 2048, &a).unwrap();
        assert!(proof.has_pixels());
        b[..4].fill(42);
        proof.observe(0, 0, 2048, &b).unwrap();
        proof.observe(1, 1024, 2048, &a).unwrap();
        b = a.clone();
        b[1020..].fill(99);
        proof.observe(1, 1024, 2048, &b).unwrap();
        b[3] ^= 1;
        assert!(
            proof
                .observe(1, 1024, 2048, &b)
                .unwrap_err()
                .is::<ChangedFrame>()
        );
        b = a.clone();
        b[4] ^= 1;
        assert!(proof.observe(0, 0, 2048, &b).is_err());
        assert!(proof.observe(0, 0, 2048, &[17; 512]).is_err());
    }
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
