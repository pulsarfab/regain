//! Trace-verified ASI676MC sensor profile. Shared lifecycle is in `bayer`.
//! SDK 1.41 map-final-full evidence: docs/direct-driver-experiments.json;
//! separate live-video evidence: docs/asi676-video.md.
use super::{asi676_tables, bayer, settings::Settings, transport::Camera};
use anyhow::Result;
use serde_json::Value;
pub const PROFILE: bayer::Profile = bayer::Profile {
    name: "ASI676MC",
    pid: 0x676d,
    width: 3552,
    height: 3552,
    offset_max: 200,
    alignment: 2,
    y_alignment: 2,
    sensor_height_alignment: 1,
    color: true,
    sensor_alignment: 1,
    // Traced bandwidth 40: 176 / 20 = 8.8 us per sensor line.
    hmax: 176,
    // Gain word 306c/306d; 3030 changes branch at gain 180. Below: gain/3;
    // at 180+: (gain-78)/3. These constants differ from the 585/662 profiles.
    gain_register: 0x306c,
    hcg_threshold: 180,
    hcg_offset: 78,
    // No model-wide short-frame floor. Shared bayer code independently applies
    // the USB 2 pacing guard; USB 3 keeps the measured SDK timing here.
    minimum_frame_lines: 0,
    initialize: asi676_tables::INITIALIZE,
    raw16: asi676_tables::RAW16_FULL,
};
pub fn capture(
    camera: &Camera,
    info: &Value,
    settings: &Settings,
    replay: bool,
) -> Result<(Value, Vec<u8>)> {
    bayer::capture(camera, info, settings, replay, &PROFILE)
}
