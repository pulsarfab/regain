//! Research ASI220MM Mini (Duo guide): distinct USB2 RAW16 acquisition.
use crate::{asi220_tables, processing, settings::Settings, transport::Camera};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn sensor(c: &Camera, register: u16, value: u16) -> Result<()> {
    c.vendor(0xb6, register, value, 0)?;
    Ok(())
}
fn word(c: &Camera, low: u16, value: u32, bytes: u16) -> Result<()> {
    for i in 0..bytes {
        sensor(c, low - i, ((value >> (8 * i)) & 255) as u16)?;
    }
    Ok(())
}
fn stop(c: &Camera) -> Result<()> {
    sensor(c, 0x100, 0)?;
    c.vendor(0xaa, 0, 0, 0)?;
    c.reset_pipe()
}
pub fn raw_settings(s: &Settings, bin: u32) -> Result<Settings> {
    ensure!(
        [1, 2].contains(&bin),
        "guide supports software bins 1 and 2"
    );
    let mut raw = s.clone();
    raw.width = s
        .width
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("guide width overflow"))?;
    raw.height = s
        .height
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("guide height overflow"))?;
    raw.x =
        s.x.checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("guide origin overflow"))?;
    raw.y =
        s.y.checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("guide origin overflow"))?;
    ensure!(
        raw.width >= 64
            && raw.width <= 1920
            && s.width.is_multiple_of(8)
            && raw.height >= 64
            && raw.height <= 1080
            && s.height.is_multiple_of(2)
            && raw.x <= 1920 - raw.width
            && raw.y <= 1080 - raw.height
            && raw.x.is_multiple_of(2)
            && raw.y.is_multiple_of(2),
        "invalid guide ROI"
    );
    ensure!(
        (32..=10_000_000).contains(&s.microseconds)
            && s.read_retries <= 5
            && s.gain <= 600
            && (200..=1500).contains(&s.offset),
        "unsupported guide exposure/gain/offset"
    );
    ensure!(
        s.interrupt_read_after_bytes == 0
            && s.replay_prefix_bytes == 0
            && s.timeout_read_after_bytes == 0,
        "guide retained-read recovery has not been established"
    );
    ensure!(
        timing(&raw).2 > 0,
        "guide zero-line integrations are not validated (SDK also failed the 32us test)"
    );
    Ok(raw)
}

/// Derived from d16e0; values are byte registers including SDK truncation.
fn gain_registers(gain: u32) -> [u16; 4] {
    let mut a = 0x3f;
    let mut b = 0x7f;
    let mut c = 0;
    let mut d = 0x80;
    let db = gain as f64 / 10.0;
    if db < 35.0 {
        let v = 10.0_f64.powf(db / 20.0);
        let mut base = 1.0;
        let mut step = 0.015625;
        for (lo, hi, coarse, fine) in [
            (1.0, 2.0, 3, 0.015625),
            (2.0, 3.4, 7, 0.03111111111111111),
            (3.4, 6.8, 0x23, 0.053125),
            (6.8, 13.6, 0x27, 0.10625),
            (13.6, 27.2, 0x2f, 0.2125),
            (27.2, 54.4, 0x3f, 0.425),
        ] {
            if (lo..hi).contains(&v) {
                a = coarse;
                base = lo;
                step = fine;
                break;
            }
        }
        b = (((v - base) / step) as u32 + 64) & 255;
    } else {
        let v = 10.0_f64.powf((db - 35.0) / 20.0);
        let mut base = 1.0;
        let mut step = 0.015625;
        for (lo, hi, coarse, fine) in [
            (1.0, 2.0, 0, 0.03125),
            (2.0, 4.0, 1, 0.0625),
            (4.0, 8.0, 3, 0.125),
            (8.0, 16.0, 7, 0.25),
            (16.0, 32.0, 15, 0.5),
        ] {
            if (lo..hi).contains(&v) {
                c = coarse;
                base = lo;
                step = fine;
                break;
            }
        }
        d = ((((v - base) / step) as u32) * 4 + 128) & 255;
    }
    [a as u16, b as u16, c as u16, d as u16]
}
fn timing(s: &Settings) -> (u32, u32, u32) {
    let hts = if s.microseconds >= 1_000_000 {
        0x1b00
    } else {
        0x840
    };
    let line = (f64::from(hts * 2) * 0.02500000037252903) as f32;
    let minimum = (s.height as f32 * line) as u32;
    let count = (s.microseconds as f32 / line) as u32;
    let frame = count.clamp(0x460, 0xffff);
    let shutter = if s.microseconds < minimum {
        if s.microseconds as f32 > 4.0 * line {
            count.saturating_sub(4)
        } else {
            0
        }
    } else {
        frame - 4
    };
    (hts, frame, shutter << 4)
}

pub fn capture(c: &Camera, info: &Value, s: &Settings, bin: u32) -> Result<(Value, Vec<u8>)> {
    let raw = raw_settings(s, bin)?;
    ensure!(
        info["productId"] == 0x2209 && info["usbVersionBcd"] == 0x200,
        "guide capture requires observed PID 2209 USB2"
    );
    let started = Instant::now();
    let result = (|| {
        stop(c)?;
        c.vendor(0xbe, 0, 0, 0)?;
        let calibration = (|| {
            let mut blob = c.vendor(0xc3, 0, 0x400, 2048)?;
            let length = processing::declared_length(&blob)?;
            while blob.len() < length {
                let at = blob.len();
                let amount = (length - at).min(2048).next_multiple_of(256);
                blob.extend(c.vendor(0xc3, 0, ((0x40000 + at) >> 8) as u16, amount as u16)?);
            }
            processing::Defects::decode_guide(
                &blob,
                raw.width as usize,
                raw.height as usize,
                raw.x as usize,
                raw.y as usize,
            )
        })();
        let restore = c.vendor(0xbe, 1, 0, 0);
        let defects = calibration?;
        restore?;
        for &(r, v) in asi220_tables::INITIALIZE {
            sensor(c, r, v)?;
        }
        sensor(c, 0x100, 0)?;
        // AC selects RAW16; B5 holds ceil(wire bytes / 48 KiB), not pixel count.
        c.vendor(0xac, 0, 0, 0)?;
        c.reset_pipe()?;
        let bytes = raw.width * raw.height * 2;
        let blocks = bytes.div_ceil(49152);
        c.vendor(0xb5, (blocks >> 16) as u16, blocks as u16, 0)?;
        word(c, 0x3201, raw.x + 4, 2)?;
        word(c, 0x3203, raw.y + 4, 2)?;
        word(c, 0x3205, raw.x + raw.width + 11, 2)?;
        word(c, 0x3207, raw.y + raw.height + 11, 2)?;
        word(c, 0x320b, raw.height, 2)?;
        word(c, 0x3209, raw.width, 2)?;
        let gain = gain_registers(s.gain);
        for (r, v) in [0x3e08, 0x3e09, 0x3e06, 0x3e07].into_iter().zip(gain) {
            sensor(c, r, v)?;
        }
        word(c, 0x3908, s.offset, 2)?;
        let (hts, frame, shutter) = timing(&raw);
        word(c, 0x320d, hts, 2)?;
        word(c, 0x320f, frame, 2)?;
        word(c, 0x3e02, shutter, 3)?;
        stop(c)?;
        c.vendor(0xaf, 0, 0, 0)?;
        c.vendor(0xa9, 0, 0, 0)?;
        sensor(c, 0x100, 1)?;
        // SDK double-buffers USB requests but returns the first complete pass;
        // a submitted next pass is not a discarded completed frame.
        let armed = Instant::now();
        let wait = s.microseconds / 1000 + 5000;
        let mut discarded = 0;
        let mut read_errors = Vec::new();
        let mut data = loop {
            let frame = match c.read_frame_wait(bytes as usize, wait) {
                Ok(frame) => frame,
                Err(error) => {
                    crate::diagnostics::read_failure(
                        "ASI220MM Mini",
                        &error,
                        discarded as usize,
                        s.read_retries.min(2),
                        false,
                    );
                    ensure!(
                        discarded < s.read_retries.min(2),
                        "guide stream did not synchronize: {error}"
                    );
                    read_errors.push(error.to_string());
                    discarded += 1;
                    continue;
                }
            };
            if frame[..4] == [0x11, 0xaa, 0, 0xbb]
                && frame[frame.len() - 4..] == [0xbb, 0, 0xaa, 0x11]
            {
                break frame;
            }
            crate::diagnostics::read_failure(
                "ASI220MM Mini",
                "invalid frame boundaries",
                discarded as usize,
                s.read_retries.min(2),
                false,
            );
            ensure!(
                discarded < s.read_retries.min(2),
                "guide frame boundaries did not synchronize"
            );
            discarded += 1;
        };
        stop(c)?;
        let last = data.len() - 4;
        let row = raw.width as usize * 2;
        data.copy_within(row..row + 4, 0);
        data.copy_within(last - row..last - row + 4, last);
        let seed = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u32;
        processing::unpack_guide(&mut data, s.gain as i32, seed)?;
        defects.correct(&mut data)?;
        if bin > 1 {
            data = processing::bin_average(
                &data,
                raw.width as usize,
                raw.height as usize,
                bin as usize,
            )?;
        }
        let meta = json!({"model":"ASI220MM Mini","sdkLoaded":false,"width":s.width,"height":s.height,
            "x":s.x,"y":s.y,"bin":bin,"gain":s.gain,"offset":s.offset,"microseconds":s.microseconds,
            "rawWidth":raw.width,"rawHeight":raw.height,"wireBytes":bytes,"bytes":data.len(),
            "sha256":format!("{:x}",Sha256::digest(&data)),"factoryDefects":defects.indices.len(),
            "discardedStartupFrames":discarded,"startupReadErrors":read_errors,"ditherSeed":seed,"acquisitionMs":armed.elapsed().as_millis(),
            "elapsedMs":started.elapsed().as_millis()});
        Ok((meta, data))
    })();
    let cleanup = stop(c);
    crate::completion::finish(result, cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guide_controls_match_independent_sdk_traces() {
        assert_eq!(gain_registers(0), [3, 64, 0, 128]);
        assert_eq!(gain_registers(100), [7, 101, 0, 128]);
        assert_eq!(gain_registers(400), [63, 127, 0, 224]);
        assert_eq!(gain_registers(349), [63, 229, 0, 128]);
        assert_eq!(gain_registers(350), [63, 127, 0, 128]);
        assert_eq!(gain_registers(600), [63, 127, 15, 140]);
        let mut settings = Settings {
            width: 512,
            height: 256,
            microseconds: 100000,
            offset: 200,
            ..Settings::default()
        };
        assert_eq!(timing(&settings), (0x840, 0x460, 0x45c0));
        settings.microseconds = 2_000_000;
        settings.width = 1920;
        settings.height = 1080;
        assert_eq!(timing(&settings), (0x1b00, 0x169b, 0x16970));
        settings.microseconds = 32;
        assert!(raw_settings(&settings, 1).is_err());
        assert!(raw_settings(&settings, 4).is_err());
    }
}
