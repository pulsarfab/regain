//! Trace-verified ASI662MC sensor profile. Shared lifecycle is in `bayer`.
use super::{asi662_tables, bayer, settings::Settings, transport::Camera};
use anyhow::Result;
use serde_json::Value;
pub const PROFILE: bayer::Profile = bayer::Profile {
    name: "ASI662MC",
    pid: 0x662b,
    width: 1920,
    height: 1080,
    offset_max: 300,
    alignment: 8,
    sensor_alignment: 16,
    hmax: 230,
    gain_register: 0x3070,
    hcg_threshold: 200,
    hcg_offset: 150,
    minimum_frame_lines: 8703,
    initialize: asi662_tables::INITIALIZE,
    raw16: asi662_tables::RAW16_FULL,
};
pub fn capture(
    camera: &Camera,
    info: &Value,
    settings: &Settings,
    replay: bool,
) -> Result<(Value, Vec<u8>)> {
    bayer::capture(camera, info, settings, replay, &PROFILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sdk_timing_gain_and_paced_short_exposures() {
        let mut s = Settings {
            width: 1920,
            height: 1080,
            ..Settings::default()
        };
        assert_eq!(s.timing_with_hmax(PROFILE.hmax), (8703, 8));
        for (gain, expected) in [
            (0, (0, 0)),
            (199, (0, 66)),
            (200, (1, 16)),
            (201, (1, 17)),
            (600, (1, 150)),
        ] {
            assert_eq!(PROFILE.gain(gain), expected);
        }
        for us in [
            32, 1000, 10000, 100000, 999999, 1000000, 60000000, 2000000000,
        ] {
            s.microseconds = us;
            let sdk = s.timing_with_hmax(PROFILE.hmax);
            let direct = PROFILE.timing(&s);
            assert_eq!(
                direct.0 - direct.1,
                sdk.0 - sdk.1,
                "integration changed at {us}"
            );
            if us >= 1000000 {
                assert_eq!(direct, sdk);
            } else {
                assert!(direct.0 >= 8703);
            }
        }
        s.microseconds = 32;
        assert_eq!(s.timing_with_hmax(PROFILE.hmax), (1140, 1130));
        assert_eq!(PROFILE.timing(&s), (8703, 8693));
    }
    #[test]
    fn rejects_unsupported_roi_and_control_ranges() {
        let mut s = Settings {
            width: 64,
            height: 64,
            x: 1856,
            y: 1016,
            offset: 300,
            ..Settings::default()
        };
        PROFILE.validate(&s).unwrap();
        s.y = 1018;
        assert!(PROFILE.validate(&s).is_err());
        s.y = 1016;
        s.width = 72;
        assert!(PROFILE.validate(&s).is_err());
        s.width = 64;
        s.offset = 301;
        assert!(PROFILE.validate(&s).is_err());
        s.offset = 15;
        s.microseconds = 2000000001;
        assert!(PROFILE.validate(&s).is_err());
    }
}
