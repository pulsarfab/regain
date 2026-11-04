//! Camera-connected CFW3. No independent handle and no automatic move replay.
use anyhow::{Result, ensure};
pub fn decode_position(byte: u8) -> Result<u8> {
    b"0123456789ABCDEF"
        .iter()
        .position(|v| *v == byte)
        .map(|v| v as u8)
        .ok_or_else(|| anyhow::anyhow!("Invalid QHY wheel response {byte:#x}"))
}
pub fn decode_slots(byte: u8) -> Result<u8> {
    // The observed camera bridge returns a count (MXP '9' -> SDK 9), not
    // maximum index. 'F' historically denotes 16 positions.
    let slots = if byte == b'F' {
        16
    } else {
        decode_position(byte)?
    };
    ensure!((2..=16).contains(&slots), "Invalid QHY wheel slot count");
    Ok(slots)
}
use std::time::{Duration, Instant};

/// A first matching reply can precede the end of settling. Match the physical
/// acceptance probe: require a second matching reply at least 250 ms later.
pub struct Motion {
    target: u8,
    started: Instant,
    matched: Option<Instant>,
}
impl Motion {
    pub fn new(target: u8, started: Instant) -> Self {
        Self {
            target,
            started,
            matched: None,
        }
    }
    pub fn observe(&mut self, position: Option<u8>, now: Instant) -> anyhow::Result<bool> {
        anyhow::ensure!(
            now.duration_since(self.started) < Duration::from_secs(45),
            regain_core::invalid("Wheel did not settle at requested position; no move replay")
        );
        if position != Some(self.target) {
            self.matched = None;
            return Ok(false);
        }
        let first = *self.matched.get_or_insert(now);
        Ok(now.duration_since(first) >= Duration::from_millis(250))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_target_and_unknown_readbacks_do_not_complete_a_move() {
        let start = Instant::now();
        let mut motion = Motion::new(4, start);
        assert!(!motion.observe(Some(4), start).unwrap());
        assert!(
            !motion
                .observe(None, start + Duration::from_millis(300))
                .unwrap()
        );
        assert!(
            !motion
                .observe(Some(3), start + Duration::from_millis(600))
                .unwrap()
        );
        assert!(
            !motion
                .observe(Some(4), start + Duration::from_millis(900))
                .unwrap()
        );
        assert!(
            !motion
                .observe(Some(4), start + Duration::from_millis(1000))
                .unwrap()
        );
        assert!(
            motion
                .observe(Some(4), start + Duration::from_millis(1200))
                .unwrap()
        );
    }
    #[test]
    fn movement_deadline_is_not_reset_by_partial_progress() {
        let start = Instant::now();
        let mut motion = Motion::new(8, start);
        assert!(
            !motion
                .observe(Some(8), start + Duration::from_secs(44))
                .unwrap()
        );
        let error = motion
            .observe(None, start + Duration::from_secs(45))
            .unwrap_err();
        assert!(!regain_core::retryable(&error));
    }
}
