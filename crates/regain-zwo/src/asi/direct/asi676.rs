//! Trace-verified ASI676MC sensor profile. Shared lifecycle is in `bayer`.
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
    hmax: 176,
    gain_register: 0x306c,
    hcg_threshold: 180,
    hcg_offset: 78,
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
