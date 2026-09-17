//! ASI6200MM Pro original and P25 acquisition over the platform USB transport.
use crate::{asi6200_tables, processing, settings::Settings, transport::Camera};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

// SDK exposure range. Integrations >= 1 s use host timing, so their duration
// does not increase the sensor frame/shutter register values.
pub const MAX_EXPOSURE_US: u32 = 2_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Revision {
    Original,
    P25,
}
impl Revision {
    pub fn from_register(value: u8) -> Result<Self> {
        // SDK 1.41 selects the faster sensor timing with BC:1c == 5.
        // Only these two register values have been tested on hardware.
        match value {
            3 => Ok(Self::Original),
            5 => Ok(Self::P25),
            _ => anyhow::bail!("unverified ASI6200 hardware revision {value:#x}; use SDK mode"),
        }
    }
    pub fn detect(camera: &Camera) -> Result<Self> {
        Self::from_register(camera.vendor(0xbc, 0x1c, 0, 1)?[0])
    }
    fn hmax(self) -> u32 {
        match self {
            Self::Original => 1515,
            Self::P25 => 880,
        }
    }
}

fn writes(camera: &Camera, commands: &[(u8, u16, u16)]) -> Result<()> {
    for &(request, register, value) in commands {
        camera.vendor(request, register, value, 0)?;
    }
    Ok(())
}
fn word(camera: &Camera, request: u8, register: u16, value: u32, bytes: u16) -> Result<()> {
    if request == 0xbd {
        camera.vendor(request, 1, 1, 0)?;
    }
    for byte in 0..bytes {
        camera.vendor(
            request,
            register + byte,
            ((value >> (8 * byte)) & 255) as u16,
            0,
        )?;
    }
    if request == 0xbd {
        camera.vendor(request, 1, 0, 0)?;
    }
    Ok(())
}
fn stop(camera: &Camera) -> Result<()> {
    let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
    camera.vendor(0xbd, 0, u16::from(flags | 0x10), 0)?;
    freeze(camera)?;
    camera.vendor(0xaa, 0, 0, 0)?;
    camera.reset_pipe()
}
fn freeze(camera: &Camera) -> Result<()> {
    // BD00 bit 10 and AA discard the retained frame on this P25 firmware.
    writes(camera, &[(0xb6, 0x19e, 5), (0xb6, 0, 5)])
}
fn frame_sequence(data: &[u8]) -> Result<u16> {
    ensure!(
        data.len() >= 16 && data[..2] == [0x7e, 0x5a] && data[data.len() - 2..] == [0xf0, 0x3c],
        "invalid ASI6200 frame envelope"
    );
    let first = u16::from_le_bytes(data[2..4].try_into().unwrap());
    let last = u16::from_le_bytes(data[data.len() - 4..data.len() - 2].try_into().unwrap());
    ensure!(first == last, "ASI6200 frame envelope counters differ");
    Ok(first)
}
pub fn validate(s: &Settings, gain: i32) -> Result<()> {
    ensure!(
        s.width >= 64
            && s.width <= 9576
            && s.width.is_multiple_of(8)
            && s.height >= 64
            && s.height <= 6388
            && s.height.is_multiple_of(2)
            && s.x.is_multiple_of(16)
            && s.y.is_multiple_of(2)
            && s.x <= 9576 - s.width
            && s.y <= 6388 - s.height,
        "invalid ASI6200 bin-1 ROI (x multiple of 16, y even)"
    );
    ensure!(
        (32..=MAX_EXPOSURE_US).contains(&s.microseconds)
            && (0..=700).contains(&gain)
            && s.offset <= 200
            && s.read_retries <= 5,
        "unsupported ASI6200 exposure/controls"
    );
    let bytes = s.width * s.height * 2;
    ensure!(
        s.interrupt_read_after_bytes == 0 || s.timeout_read_after_bytes == 0,
        "choose one read fault injection"
    );
    for prefix in [
        s.interrupt_read_after_bytes,
        s.replay_prefix_bytes,
        s.timeout_read_after_bytes,
    ] {
        ensure!(
            prefix == 0 || (prefix >= 1024 && prefix < bytes && prefix.is_multiple_of(1024)),
            "ASI6200 interruption prefix must be packet aligned and shorter than the frame"
        );
    }
    Ok(())
}
fn calibration(camera: &Camera, s: &Settings) -> Result<processing::Defects> {
    camera.vendor(0xbe, 0, 0, 0)?;
    let result = (|| {
        let mut data = camera.vendor(0xc3, 0, 0x400, 2048)?;
        let length = processing::declared_length(&data)?;
        while data.len() < length {
            let offset = data.len();
            let amount = (length - offset).min(2048).next_multiple_of(256);
            data.extend(camera.vendor(0xc3, 0, ((0x40000 + offset) >> 8) as u16, amount as u16)?);
        }
        processing::Defects::decode_6200(
            &data,
            s.width as usize,
            s.height as usize,
            s.x as usize,
            s.y as usize,
        )
    })();
    let restore = camera.vendor(0xbe, 1, 0, 0);
    let defects = result?;
    restore?;
    Ok(defects)
}

pub fn gain_registers(gain: i32) -> (u32, u16, u16, u16) {
    let digital = if gain > 460 {
        (gain - 460 + 59) / 60
    } else {
        0
    };
    let adjusted = if gain < 100 {
        gain
    } else {
        gain - 100 - digital * 60
    };
    // Preserve the SDK's division and subtraction order at integer boundaries.
    let analog = (4095.0 - 4095.0 * 10.0_f64.powf(-(adjusted as f64 / 10.0 / 20.0))) as u32;
    let (mode, readout) = match gain {
        ..61 => (0, 8),
        61..100 => (4, 10),
        100..160 => (1, 8),
        160..280 => (5, 10),
        _ => (5, 12),
    };
    (analog, mode, readout, (digital * 16) as u16)
}
pub fn timing(s: &Settings, revision: Revision) -> (u32, u32) {
    let line = revision.hmax() as f32 * 1000.0 / 20000.0;
    let minimum = ((s.height + 52) as f32 * line) as u32;
    let exposure = if s.microseconds >= 1_000_000 {
        minimum + 10000
    } else {
        s.microseconds
    };
    let (frame, shutter) = if exposure > minimum {
        ((exposure as f32 / line) as u32 + 20, 20)
    } else {
        let frame = s.height + 52;
        (
            frame,
            frame
                .saturating_sub((exposure as f32 / line) as u32 + 3)
                .clamp(3, frame - 3),
        )
    };
    (frame.min(0xffffff), (shutter.min(0x1fffe) / 2).max(3))
}
fn begin_retained_read(camera: &Camera) -> Result<()> {
    camera.phase("restarting_retained");
    camera.vendor(0xbd, 0x18, 0, 0)?;
    camera.service_environment()?;
    std::thread::sleep(Duration::from_millis(100));
    // Stop the sender before clearing queued USB data. Resetting while the
    // P25 sender is still active can leave a stale partial pass in the pipe.
    camera.reset_pipe()?;
    ensure!(
        camera.vendor(0xbc, 0x18, 0, 1)?[0] == 0,
        "ASI6200 replay engine did not return idle"
    );
    let retained = camera.vendor(0xbc, 0x23, 0, 1)?[0];
    ensure!(
        retained & !0x10 == 5,
        "ASI6200 retained frame was lost: {retained:#x}"
    );
    camera.vendor(0xbd, 0x18, 1, 0)?;
    camera.phase("retained");
    Ok(())
}

pub fn raw_settings(s: &Settings, gain: i32, bin: u32) -> Result<Settings> {
    ensure!((1..=4).contains(&bin), "ASI6200 bin must be 1..4");
    ensure!(
        s.width.is_multiple_of(8) && s.height.is_multiple_of(2),
        "ASI6200 output dimensions require width multiple of 8 and even height"
    );
    let mut raw = s.clone();
    raw.width = s
        .width
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("ASI6200 width overflow"))?;
    raw.height = s
        .height
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("ASI6200 height overflow"))?;
    raw.x =
        s.x.checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("ASI6200 x overflow"))?;
    raw.y =
        s.y.checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("ASI6200 y overflow"))?;
    validate(&raw, gain)?;
    Ok(raw)
}
pub fn capture(
    camera: &Camera,
    info: &Value,
    s: &Settings,
    gain: i32,
    bin: u32,
    replay: bool,
) -> Result<(Value, Vec<u8>)> {
    let raw = raw_settings(s, gain, bin)?;
    let sensor = sensor_settings(&raw);
    let (mut meta, mut data) = capture_native(camera, info, &sensor, gain, replay)?;
    if sensor.width != raw.width || sensor.height != raw.height {
        let mut cropped = Vec::with_capacity(raw.width as usize * raw.height as usize * 2);
        for row in 0..raw.height {
            let start = ((row + raw.y - sensor.y) * sensor.width + raw.x - sensor.x) as usize * 2;
            cropped.extend_from_slice(&data[start..start + raw.width as usize * 2]);
        }
        data = cropped;
    }
    if bin > 1 {
        data =
            processing::bin_average(&data, raw.width as usize, raw.height as usize, bin as usize)?;
    }
    meta["wireBytes"] = json!(sensor.width * sensor.height * 2);
    meta["rawWidth"] = json!(sensor.width);
    meta["rawHeight"] = json!(sensor.height);
    meta["rawX"] = json!(sensor.x);
    meta["rawY"] = json!(sensor.y);
    meta["width"] = json!(s.width);
    meta["height"] = json!(s.height);
    meta["bin"] = json!(bin);
    meta["x"] = json!(s.x);
    meta["y"] = json!(s.y);
    meta["bytes"] = json!(data.len());
    meta["sha256"] = json!(format!("{:x}", Sha256::digest(&data)));
    Ok((meta, data))
}
fn sensor_settings(raw: &Settings) -> Settings {
    let mut sensor = raw.clone();
    // This P25 unit stalls DDR replay for 8/16/32/64 KiB frames. Both narrow
    // and wide 128 KiB frames replay correctly. Read at least that amount,
    // correct defects in sensor coordinates, then crop before software binning.
    if sensor.width * sensor.height < 65536 {
        sensor.width = sensor.width.max(512);
        sensor.height = sensor.height.max(128);
        sensor.x = sensor.x.min((9576 - sensor.width) / 16 * 16);
        sensor.y = sensor.y.min(6388 - sensor.height);
    }
    sensor
}
fn capture_native(
    camera: &Camera,
    info: &Value,
    s: &Settings,
    gain: i32,
    replay: bool,
) -> Result<(Value, Vec<u8>)> {
    validate(s, gain)?;
    ensure!(
        info["productId"] == 0x620b && info["usbVersionBcd"] == 0x300,
        "ASI6200 research capture requires observed PID 620b USB3"
    );
    let started = Instant::now();
    let revision = Revision::detect(camera)?;
    camera.phase("initializing");
    let result = (|| {
        for &(request, register, value) in asi6200_tables::INITIALIZE {
            // Host cooling/dew state must survive sensor initialization between frames.
            if camera.has_environment() && request == 0xbd && [0x19, 0x26].contains(&register) {
                continue;
            }
            // The remaining differences in the observed initialization are
            // saved gain/offset/exposure values, explicitly set below.
            let value = match (request, register) {
                (0xbd, 0x13) => (revision.hmax() & 255) as u16,
                (0xbd, 0x14) => (revision.hmax() >> 8) as u16,
                _ => value,
            };
            camera.vendor(request, register, value, 0)?;
        }
        let defects = calibration(camera, s)?;
        writes(camera, asi6200_tables::RAW16)?;
        word(camera, 0xbd, 6, 49, 2)?;
        word(camera, 0xbd, 2, 24, 2)?;
        writes(camera, &[(0xb6, 0xa5, 1), (0xb6, 5, 1)])?;
        word(camera, 0xb6, 0xa6, s.x / 16, 2)?;
        word(camera, 0xb6, 6, s.y + 25, 2)?;
        word(camera, 0xbd, 0x40, s.width * s.height / 2, 4)?;
        camera.vendor(0xb6, 0x187, 4, 0)?;
        word(camera, 0xb6, 8, s.height, 2)?;
        word(camera, 0xb6, 0x18c, s.width + 24, 2)?;
        word(camera, 0xbd, 8, s.height, 2)?;
        word(camera, 0xbd, 4, s.width, 2)?;
        // Both revisions use divisor 400 at USB bandwidth 40.
        word(camera, 0xbd, 0x13, revision.hmax(), 2)?;
        word(camera, 0xbd, 0x24, 400, 2)?;
        let (analog, mode, readout, digital) = gain_registers(gain);
        writes(
            camera,
            &[
                (0xb6, 0x2d, mode),
                (0xb6, 0x4d, readout),
                (0xb6, 0x3a2, 7),
                (0xb6, 0x3a3, 17),
                (0xb6, 0x3a4, if gain >= 280 { 35 } else { 17 }),
                (0xb6, 0x3a5, if gain >= 280 { 45 } else { 17 }),
                (0xb6, 0x3a6, if gain >= 280 { 45 } else { 17 }),
            ],
        )?;
        word(camera, 0xb6, 0x2e, analog, 2)?;
        word(camera, 0xb6, 0x30, analog, 2)?;
        camera.vendor(0xb6, 0x3e, digital, 0)?;
        word(camera, 0xb6, 0x40, s.offset * 10, 2)?;
        word(camera, 0xb6, 0x42, s.offset * 10, 2)?;
        let (frame, shutter) = timing(s, revision);
        word(camera, 0xbd, 0x10, frame, 3)?;
        word(camera, 0xb6, 0x16, shutter, 2)?;
        let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
        camera.vendor(
            0xbd,
            0,
            u16::from(if s.microseconds >= 1_000_000 {
                flags | 0xc0
            } else {
                flags & !0xc0
            }),
            0,
        )?;
        stop(camera)?;
        let old = camera.vendor(0xbc, 0x23, 0, 1)?[0];
        ensure!(old == 1, "ASI6200 old frame did not clear: {old}");
        let armed = Instant::now();
        camera.phase("exposing");
        writes(camera, &[(0xa9, 0, 0), (0xb6, 0x19e, 1), (0xb6, 0, 5)])?;
        std::thread::sleep(Duration::from_millis(50));
        camera.vendor(0xb6, 0, 4, 0)?;
        let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
        camera.vendor(0xbd, 0, u16::from(flags & !0x10), 0)?;
        camera.reset_pipe()?;
        if s.microseconds >= 1_000_000 {
            // SDK 1.41 worker 1ed2cc..1ed60b: synchronize the FPGA, then
            // gate sensor/FPGA clocks during the long integration.
            let mut synchronized = false;
            for _ in 0..5 {
                let f = camera.vendor(0xbc, 0, 0, 1)?[0];
                camera.vendor(0xbd, 0, u16::from(f | 0x10), 0)?;
                camera.service_environment()?;
                std::thread::sleep(Duration::from_millis(5));
                camera.vendor(0xbd, 0, u16::from(f & !0x10), 0)?;
                std::thread::sleep(Duration::from_millis(20));
                if camera.vendor(0xbc, 0x23, 0, 1)?[0] & 0x10 != 0 {
                    synchronized = true;
                    break;
                }
            }
            ensure!(synchronized, "ASI6200 long exposure synchronization failed");
            let f = camera.vendor(0xbc, 0xb, 0, 1)?[0];
            camera.vendor(0xbd, 0xb, u16::from(f | 1), 0)?;
            let exposure_ms = s.microseconds / 1000;
            if exposure_ms > 1000 {
                let trigger = Instant::now();
                let mut iteration = 0;
                loop {
                    match iteration {
                        6 => {
                            camera.vendor(0xb6, 0x19e, 5, 0)?;
                        }
                        8 => {
                            let f = camera.vendor(0xbc, 0x19, 0, 1)?[0];
                            camera.vendor(0xbd, 0x19, u16::from(f | 1), 0)?;
                        }
                        10 => {
                            let f = camera.vendor(0xbc, 0xb, 0, 1)?[0];
                            camera.vendor(0xbd, 0xb, u16::from(f | 0x10), 0)?;
                        }
                        _ => {}
                    }
                    let elapsed = trigger.elapsed().as_millis();
                    camera.service_environment()?;
                    std::thread::sleep(Duration::from_millis(100));
                    iteration += 1;
                    if elapsed >= u128::from(exposure_ms - 200) {
                        break;
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(u64::from(exposure_ms - 200)));
            }
            let f = camera.vendor(0xbc, 0x19, 0, 1)?[0];
            camera.vendor(0xbd, 0x19, u16::from(f & !1), 0)?;
            camera.service_environment()?;
            std::thread::sleep(Duration::from_millis(100));
            camera.vendor(0xb6, 0x19e, 1, 0)?;
            camera.service_environment()?;
            std::thread::sleep(Duration::from_millis(100));
            let f = camera.vendor(0xbc, 0xb, 0, 1)?[0];
            camera.vendor(0xbd, 0xb, u16::from(f & !0x10), 0)?;
            camera.vendor(0xbd, 0xb, u16::from(f & !0x11), 0)?;
        }
        let deadline = Duration::from_micros(u64::from(s.microseconds)) + Duration::from_secs(10);
        loop {
            let status = camera.vendor(0xbc, 0x23, 0, 1)?[0];
            if status == 0x15 {
                break;
            }
            ensure!(
                armed.elapsed() < deadline,
                "ASI6200 retained-frame timeout: {status}"
            );
            camera.service_environment()?;
            std::thread::sleep(Duration::from_millis(5));
        }
        // BC23=15 announces readout, not completion of all sensor rows.
        // Allow the complete programmed frame (HMAX / 20 MHz per row),
        // plus a settling margin, before stopping the sensor. The ready bit
        // alone can freeze partially refreshed DDR with a valid envelope.
        let readout = Duration::from_micros(
            (u64::from(frame) * u64::from(revision.hmax())).div_ceil(20) + 100000,
        );
        camera.phase("sensor_readout");
        let settling = Instant::now();
        while settling.elapsed() < readout {
            camera.service_environment()?;
            std::thread::sleep(Duration::from_millis(10));
        }
        freeze(camera)?;
        camera.phase("retained");
        // Short mode streams while DDR fills; discard the queued USB pass and
        // read frozen DDR. Long mode schedules one pass, which must be consumed.
        if s.microseconds < 1_000_000 {
            begin_retained_read(camera)?;
        }
        let mut prefix = Vec::new();
        let mut read_errors = Vec::new();
        let mut data = loop {
            let attempt = (|| {
                if read_errors.is_empty() && s.timeout_read_after_bytes > 0 {
                    prefix = camera.read_frame(s.timeout_read_after_bytes as usize)?;
                    // Research CLI only: stop the retained-frame sender and drain
                    // the pipe. The following real bulk read has no producer and
                    // must expire through the normal request/cancel/drain path.
                    camera.vendor(0xbd, 0x18, 0, 0)?;
                    camera.reset_pipe()?;
                    camera.read_frame(1024 * 1024)?;
                    anyhow::bail!("stalled sender unexpectedly delivered a bulk read");
                }
                if read_errors.is_empty() && s.interrupt_read_after_bytes > 0 {
                    prefix = camera.read_frame(s.interrupt_read_after_bytes as usize)?;
                    anyhow::bail!("injected host interruption after {} bytes", prefix.len());
                }
                let data = camera.read_frame(s.width as usize * s.height as usize * 2)?;
                frame_sequence(&data)?;
                Ok(data)
            })();
            match attempt {
                Ok(data) => break data,
                Err(error) => {
                    crate::diagnostics::read_failure(
                        "ASI6200MM Pro",
                        &error,
                        read_errors.len(),
                        s.read_retries,
                        true,
                    );
                    if read_errors.len() >= s.read_retries as usize {
                        return Err(error);
                    }
                    read_errors.push(error.to_string());
                    begin_retained_read(camera)?;
                }
            }
        };
        ensure!(
            prefix.is_empty() || data[4..prefix.len()] == prefix[4..],
            "ASI6200 recovered pixels do not match interrupted prefix"
        );
        let sequence = frame_sequence(&data)?;
        camera.phase("validating");
        let wire_hash = format!("{:x}", Sha256::digest(&data));
        let replay_result = if replay {
            begin_retained_read(camera)?;
            if s.replay_prefix_bytes > 0 {
                let prefix = camera.read_frame(s.replay_prefix_bytes as usize)?;
                ensure!(
                    prefix[4..] == data[4..prefix.len()],
                    "ASI6200 replay prefix changed"
                );
                begin_retained_read(camera)?;
            }
            let mut errors = Vec::new();
            let second = loop {
                match camera.read_frame(data.len()).and_then(|frame| {
                    frame_sequence(&frame)?;
                    Ok(frame)
                }) {
                    Ok(frame) => break frame,
                    Err(e) if errors.len() < s.read_retries as usize => {
                        errors.push(e.to_string());
                        begin_retained_read(camera)?;
                    }
                    Err(e) => return Err(e),
                }
            };
            let replay_sequence = frame_sequence(&second)?;
            ensure!(
                second[4..second.len() - 4] == data[4..data.len() - 4],
                "ASI6200 retained pixels differ from original"
            );
            json!({"pixelBytesIdentical":true,"wireBytesIdentical":second==data,"bytes":data.len(),
                "firstSequence":sequence,"replaySequence":replay_sequence,"readErrors":errors})
        } else {
            Value::Null
        };
        let last = data.len() - 4;
        let row = s.width as usize * 2;
        data.copy_within(row..row + 4, 0);
        data.copy_within(last - row..last - row + 4, last);
        defects.correct(&mut data)?;
        let meta = json!({"model":"ASI6200MM Pro","width":s.width,"height":s.height,"x":s.x,"y":s.y,
            "bin":1,"gain":gain,"offset":s.offset,"microseconds":s.microseconds,"bytes":data.len(),
            "wireSha256":wire_hash,"sha256":format!("{:x}",Sha256::digest(&data)),"sequence":sequence,
            "factoryDefects":defects.indices.len(),"acquisitionMs":armed.elapsed().as_millis(),"elapsedMs":started.elapsed().as_millis(),
            "readoutGuardUs":readout.as_micros(),"sensorHmax":revision.hmax(),
            "hardwareRevision":if revision == Revision::P25 {5} else {3},
            "replay":replay_result,"sdkLoaded":false,"readRecoveries":read_errors.len(),"readErrors":read_errors,
            "interruptedPrefixBytes":prefix.len(),"interruptedPrefixPixelsMatch":!prefix.is_empty(),
            "timeoutInjectionBytes":s.timeout_read_after_bytes});
        Ok((meta, data))
    })();
    let cleanup = stop(camera);
    crate::completion::finish(result, cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_timing_matches_independent_sdk_trace() {
        // Non-P25 Camera Kit 20260917T205753Z: BC1c=3, HMAX=1515.
        for (height, microseconds, expected) in [
            (6388, 100000, (6440, 2558)),
            (256, 32, (308, 152)),
            (256, 100, (308, 152)),
            (256, 1000, (308, 146)),
            (256, 10000, (308, 86)),
            (256, 490000, (6488, 10)),
            (256, 999000, (13208, 10)),
            (256, 1000000, (460, 10)),
            (256, 60000000, (460, 10)),
        ] {
            let settings = Settings {
                height,
                microseconds,
                ..Settings::default()
            };
            assert_eq!(timing(&settings, Revision::Original), expected);
        }
        assert_eq!(Revision::from_register(3).unwrap(), Revision::Original);
        assert_eq!(Revision::from_register(5).unwrap(), Revision::P25);
        for unknown in [0, 1, 2, 4, 6, 255] {
            assert!(Revision::from_register(unknown).is_err());
        }
    }

    #[test]
    fn small_edge_rois_remain_inside_padded_sensor_readout() {
        for bin in 1..=4 {
            for (width, height) in [(64, 64), (64, 128), (128, 64), (512, 64)] {
                let settings = Settings {
                    width,
                    height,
                    x: ((9576 / bin - width) / 16) * 16,
                    y: ((6388 / bin - height) / 2) * 2,
                    ..Settings::default()
                };
                let raw = raw_settings(&settings, 100, bin).unwrap();
                let padded = sensor_settings(&raw);
                validate(&padded, 100).unwrap();
                assert!(padded.width * padded.height >= 65536);
                assert!(padded.x <= raw.x && padded.y <= raw.y);
                assert!(padded.x + padded.width >= raw.x + raw.width);
                assert!(padded.y + padded.height >= raw.y + raw.height);
            }
        }
    }

    #[test]
    fn timing_matches_sdk_141_observations() {
        let settings = Settings {
            width: 512,
            height: 256,
            microseconds: 100000,
            ..Settings::default()
        };
        assert_eq!(timing(&settings, Revision::P25), (0x8f4, 10));
        assert_eq!(
            timing(
                &Settings {
                    microseconds: 2_000_000,
                    ..settings
                },
                Revision::P25
            ),
            (555, 10)
        );
    }
    #[test]
    fn gain_boundaries_match_independent_sdk_141_trace() {
        // Observed register values from asi6200-sdk-controls, not generated
        // through this implementation. Covers every conversion/digital boundary.
        for (gain, analog, mode, readout, digital) in [
            (0, 0, 0, 8, 0),
            (60, 2042, 0, 8, 0),
            (61, 2066, 4, 10, 0),
            (62, 2089, 4, 10, 0),
            (99, 2785, 4, 10, 0),
            (100, 0, 1, 8, 0),
            (101, 46, 1, 8, 0),
            (159, 2018, 1, 8, 0),
            (160, 2042, 5, 10, 0),
            (161, 2066, 5, 10, 0),
            (279, 3573, 5, 10, 0),
            (280, 3579, 5, 12, 0),
            (281, 3585, 5, 12, 0),
            (460, 4030, 5, 12, 0),
            (461, 3966, 5, 12, 16),
            (520, 4030, 5, 12, 16),
            (521, 3966, 5, 12, 32),
            (700, 4030, 5, 12, 64),
        ] {
            assert_eq!(
                gain_registers(gain),
                (analog, mode, readout, digital),
                "gain {gain}"
            );
        }
    }

    #[test]
    fn long_exposures_keep_host_timed_sensor_registers_and_sdk_range() {
        let mut settings = Settings {
            width: 9576,
            height: 6388,
            microseconds: 1_000_000,
            ..Settings::default()
        };
        let registers = timing(&settings, Revision::P25);
        for duration in [
            30_000_001,
            60_000_000,
            120_000_000,
            1_200_000_000,
            MAX_EXPOSURE_US,
        ] {
            settings.microseconds = duration;
            assert!(validate(&settings, 100).is_ok());
            assert_eq!(timing(&settings, Revision::P25), registers);
        }
        settings.microseconds = MAX_EXPOSURE_US + 1;
        assert!(validate(&settings, 100).is_err());
    }
}
