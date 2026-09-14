use anyhow::{Result, ensure};

#[derive(Clone, Debug)]
pub struct Settings {
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    pub microseconds: u32,
    pub gain: u32,
    pub offset: u32,
    pub replay_prefix_bytes: u32,
    pub interrupt_read_after_bytes: u32,
    pub timeout_read_after_bytes: u32,
    pub read_retries: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            width: 3552,
            height: 3552,
            x: 0,
            y: 0,
            microseconds: 100_000,
            gain: 0,
            offset: 10,
            replay_prefix_bytes: 0,
            interrupt_read_after_bytes: 0,
            timeout_read_after_bytes: 0,
            read_retries: 2,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.timeout_read_after_bytes == 0,
            "timeout injection is ASI2600-only"
        );
        ensure!(
            self.width >= 64
                && self.height >= 64
                && self.width.is_multiple_of(8)
                && self.height.is_multiple_of(2)
                && self.width <= 3552
                && self.height <= 3552
                && self.x <= 3552 - self.width
                && self.y <= 3552 - self.height
                && self.x.is_multiple_of(2)
                && self.y.is_multiple_of(2),
            "invalid research bin-1 Bayer ROI (minimum 64x64, even origin, width multiple of 8, even height)"
        );
        ensure!(
            self.replay_prefix_bytes.is_multiple_of(1024)
                && self.replay_prefix_bytes < self.width * self.height * 2,
            "replay prefix must be a multiple of 1024 bytes smaller than the frame"
        );
        ensure!(
            self.read_retries <= 5
                && self.interrupt_read_after_bytes.is_multiple_of(1024)
                && self.interrupt_read_after_bytes < self.width * self.height * 2,
            "read retries must be 0..5; interrupted prefix must be a multiple of 1024 smaller than the frame"
        );
        ensure!(
            (32..=30_000_000).contains(&self.microseconds)
                && self.gain <= 600
                && self.offset <= 200,
            "research limits: exposure 32us..30s, gain 0..600, offset 0..200"
        );
        Ok(())
    }
    pub fn long_exposure(&self) -> bool {
        self.microseconds >= 1_000_000
    }

    // SDK 1.41 ASI676MC timing, HMAX=176 and clock=20000 at USB limit 40.
    // Keep f32 arithmetic: the SDK rounds through scalar single precision.
    pub fn timing(&self) -> (u32, u32) {
        let line_us = 176_f32 * 1000.0 / 20000.0;
        let minimum_us = ((self.height + 60) as f32 * line_us) as u32;
        let requested = if self.long_exposure() {
            minimum_us + 10_000
        } else {
            self.microseconds
        };
        let lines = (requested as f32 / line_us) as u32;
        if requested > minimum_us {
            (lines + 8, 8)
        } else {
            let frame = self.height + 60;
            (frame, frame.saturating_sub(lines + 8).clamp(8, frame - 8))
        }
    }
    pub fn gain_registers(&self) -> (u16, u16) {
        if self.gain >= 180 {
            (1, ((self.gain - 78) / 3) as u16)
        } else {
            (0, (self.gain / 3) as u16)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timing_matches_observed_sdk_control_transactions() {
        let mut s = Settings {
            width: 512,
            height: 256,
            ..Settings::default()
        };
        for (us, frame, shutter) in [
            (1000, 316, 195),
            (10000, 1144, 8),
            (100000, 11371, 8),
            (200000, 22735, 8),
            (500000, 56826, 8),
            (1000000, 1460, 8),
        ] {
            s.microseconds = us;
            assert_eq!(s.timing(), (frame, shutter));
        }
        s.height = 3552;
        assert_eq!(s.timing(), (4756, 8));
        s.gain = 100;
        assert_eq!(s.gain_registers(), (0, 33));
        s.gain = 300;
        assert_eq!(s.gain_registers(), (1, 74));
    }
    #[test]
    fn invalid_settings_rejected_before_hardware_access() {
        let mut s = Settings::default();
        assert!(s.validate().is_ok());
        s.x = u32::MAX;
        assert!(s.validate().is_err());
        s.x = 0;
        s.microseconds = 30_000_001;
        assert!(s.validate().is_err());
        s.microseconds = 32;
        s.width = 3551;
        assert!(s.validate().is_err());
    }
}
