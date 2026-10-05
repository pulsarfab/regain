//! Trace-verified ASI585MM Pro RAW16 profile; shared transport and acquisition.
use super::{asi585_tables, bayer, settings::Settings, transport::Camera};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub const PROFILE: bayer::Profile = bayer::Profile {
    name: "ASI585MM Pro",
    pid: 0x585e,
    width: 3840,
    height: 2160,
    offset_max: 300,
    alignment: 2,
    y_alignment: 4,
    sensor_alignment: 16,
    sensor_height_alignment: 4,
    color: false,
    hmax: 192,
    gain_register: 0x306c,
    hcg_threshold: 200,
    hcg_offset: 150,
    minimum_frame_lines: 10424,
    initialize: asi585_tables::INITIALIZE,
    raw16: asi585_tables::RAW16_FULL,
};
pub fn capture(
    camera: &Camera,
    info: &Value,
    settings: &Settings,
    bin: u32,
    replay: bool,
) -> Result<(Value, Vec<u8>)> {
    let raw = raw_settings(settings, bin)?;
    finish(
        bayer::capture(camera, info, &raw, replay, &PROFILE)?,
        settings,
        bin,
    )
}

pub fn raw_settings(settings: &Settings, bin: u32) -> Result<Settings> {
    ensure!(
        (1..=4).contains(&bin),
        "ASI585MM Pro supports software bins 1..4"
    );
    ensure!(
        settings.width >= 64
            && settings.height >= 64
            && settings.width.is_multiple_of(8)
            && settings.height.is_multiple_of(2),
        "invalid binned ROI"
    );
    let mut raw = settings.clone();
    for value in [&mut raw.width, &mut raw.height, &mut raw.x, &mut raw.y] {
        *value = value
            .checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("ROI overflow"))?;
    }
    PROFILE.validate(&raw)?;
    Ok(raw)
}

/// Factory correction runs on sensor pixels before SDK-equivalent averaging.
pub fn finish(
    (mut metadata, mut data): (Value, Vec<u8>),
    settings: &Settings,
    bin: u32,
) -> Result<(Value, Vec<u8>)> {
    let raw = raw_settings(settings, bin)?;
    if bin > 1 {
        data = super::processing::bin_average(
            &data,
            raw.width as usize,
            raw.height as usize,
            bin as usize,
        )?;
    }
    for (key, value) in [
        ("width", settings.width),
        ("height", settings.height),
        ("x", settings.x),
        ("y", settings.y),
        ("bin", bin),
    ] {
        metadata[key] = json!(value);
    }
    if bin > 1 && metadata.get("mean").is_some() {
        let pixels = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]));
        let (sum, min, max, nonzero) =
            pixels.fold((0u64, u16::MAX, 0, 0usize), |(sum, min, max, n), p| {
                (
                    sum + u64::from(p),
                    min.min(p),
                    max.max(p),
                    n + usize::from(p != 0),
                )
            });
        metadata["minimum"] = json!(min);
        metadata["maximum"] = json!(max);
        metadata["nonzeroPixels"] = json!(nonzero);
        metadata["mean"] = json!(sum as f64 / (data.len() / 2) as f64);
    }
    metadata["bytes"] = json!(data.len());
    metadata["sha256"] = json!(format!("{:x}", Sha256::digest(&data)));
    Ok((metadata, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_geometry_gain_and_binning_boundaries() {
        for (gain, expected) in [
            (0, (0, 0)),
            (199, (0, 66)),
            (200, (1, 16)),
            (201, (1, 17)),
            (600, (1, 150)),
        ] {
            assert_eq!(PROFILE.gain(gain), expected);
        }
        for bin in 1..=4 {
            let settings = Settings {
                width: 3840 / bin,
                height: 2160 / bin,
                ..Settings::default()
            };
            let raw = raw_settings(&settings, bin).unwrap();
            assert_eq!((raw.width, raw.height), (3840, 2160));
        }
        let mut roi = Settings {
            width: 520,
            height: 258,
            x: 18,
            y: 12,
            ..Settings::default()
        };
        assert!(raw_settings(&roi, 1).is_ok());
        roi.y = 2;
        assert!(raw_settings(&roi, 1).is_err());
        roi.x = u32::MAX;
        assert!(raw_settings(&roi, 4).is_err());
    }

    #[test]
    fn mono_envelope_uses_adjacent_rows_and_output_describes_binned_pixels() {
        let mut pixels: Vec<u8> = (0..64 * 64u16).flat_map(u16::to_le_bytes).collect();
        let before = pixels.clone();
        PROFILE.replace_envelope(&mut pixels, 64).unwrap();
        assert_eq!(&pixels[..4], &before[128..132]);
        assert_eq!(&pixels[8188..], &before[8060..8064]);
        assert_eq!(&pixels[4..8188], &before[4..8188]);
        let settings = Settings {
            width: 64,
            height: 64,
            ..Settings::default()
        };
        let (meta, data) = finish((json!({"bytes":32768}), vec![0; 32768]), &settings, 2).unwrap();
        assert_eq!(data.len(), 8192);
        assert_eq!(meta["bytes"], 8192);
        assert_eq!(meta["bin"], 2);
    }
}
