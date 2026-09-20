//! ASI2600MM acquisition over the platform USB transport.
use crate::{
    asi2600_p25_tables, asi2600_tables, processing, settings::Settings, transport::Camera,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

// SDK exposure range. Integrations >= 1 s use host timing, so their duration
// does not increase the sensor frame/shutter register values.
pub const MAX_EXPOSURE_US: u32 = 2_000_000_000;

#[derive(Clone, Copy, PartialEq)]
enum Revision {
    Original,
    P25,
}
impl Revision {
    fn hmax(self) -> u32 {
        match self {
            Self::Original => 779,
            Self::P25 => 790,
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
    writes(
        camera,
        &[
            (0xbd, 0, u16::from(flags | 0x10)),
            (0xb6, 0x1ee, 5),
            (0xb6, 0, 5),
            (0xaa, 0, 0),
        ],
    )?;
    camera.reset_pipe()
}
fn frame_sequence(data: &[u8]) -> Result<u16> {
    ensure!(
        data.len() >= 16 && data[..2] == [0x7e, 0x5a] && data[data.len() - 2..] == [0xf0, 0x3c],
        "invalid Duo frame envelope"
    );
    let first = u16::from_le_bytes(data[2..4].try_into().unwrap());
    let last = u16::from_le_bytes(data[data.len() - 4..data.len() - 2].try_into().unwrap());
    ensure!(first == last, "Duo frame envelope counters differ");
    Ok(first)
}
pub fn validate(s: &Settings, gain: i32) -> Result<()> {
    ensure!(
        s.width >= 64
            && s.width <= 6248
            && s.width.is_multiple_of(8)
            && s.height >= 64
            && s.height <= 4176
            && s.height.is_multiple_of(2)
            && s.x.is_multiple_of(16)
            && s.y.is_multiple_of(2)
            && s.x <= 6248 - s.width
            && s.y <= 4176 - s.height,
        "invalid Duo bin-1 ROI (x multiple of 16, y even)"
    );
    ensure!(
        (32..=MAX_EXPOSURE_US).contains(&s.microseconds)
            && (-25..=700).contains(&gain)
            && s.offset <= 240
            && s.read_retries <= 5,
        "unsupported Duo exposure/controls"
    );
    let bytes = s.width * s.height * 2;
    ensure!(
        s.reopen_delay_ms <= 10000,
        "reopen delay must be at most 10000 ms"
    );
    ensure!(
        s.reopen_after_bytes == 0
            || (s.interrupt_read_after_bytes == 0
                && s.timeout_read_after_bytes == 0
                && s.read_retries > 0),
        "reopen experiment requires read retries and no other injected fault"
    );
    ensure!(
        s.interrupt_read_after_bytes == 0 || s.timeout_read_after_bytes == 0,
        "choose one read fault injection"
    );
    for prefix in [
        s.interrupt_read_after_bytes,
        s.replay_prefix_bytes,
        s.timeout_read_after_bytes,
        s.reopen_after_bytes,
    ] {
        ensure!(
            prefix == 0 || (prefix >= 1024 && prefix < bytes && prefix.is_multiple_of(1024)),
            "Duo interruption prefix must be packet aligned and shorter than the frame"
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
        processing::Defects::decode_duo(
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

pub fn gain_registers(gain: i32) -> (u16, u32, u16, u16) {
    let digital = if gain > 460 {
        ((gain - 460) + 59) / 60
    } else {
        0
    };
    let adjusted = if gain < 0 {
        gain + 25
    } else if gain < 100 {
        gain
    } else {
        gain - 100 - digital * 60
    };
    let analog = (4095.0 * (1.0 - 10.0_f64.powf(-(adjusted as f64) / 200.0))) as u32;
    (
        if gain < 0 { 0x11 } else { 0 },
        analog,
        u16::from(gain >= 100),
        (digital * 16) as u16,
    )
}
fn timing(s: &Settings, revision: Revision) -> (u32, u32) {
    let line = revision.hmax() as f32 * 1000.0 / 20000.0;
    let minimum = ((s.height + 48) as f32 * line) as u32;
    let exposure = if s.microseconds >= 1_000_000 {
        minimum + 5000
    } else {
        s.microseconds
    };
    let (frame, shutter) = if exposure > minimum {
        ((exposure as f32 / line) as u32 + 1, 1)
    } else {
        let frame = s.height + 48;
        (
            frame,
            (frame - 1)
                .saturating_sub((exposure as f32 / line) as u32)
                .clamp(1, frame - 1),
        )
    };
    (frame.min(0xffffff), (shutter.min(0x1fffe) / 2).max(1))
}
fn begin_retained_read(camera: &Camera, revision: Revision) -> Result<()> {
    camera.phase("restarting_retained");
    if revision == Revision::Original {
        camera.reset_pipe()?;
    }
    camera.vendor(0xbd, 0x18, 0, 0)?;
    camera.service_environment()?;
    std::thread::sleep(Duration::from_millis(100));
    if revision == Revision::P25 {
        // P25 DDR sender must stop before draining the previous transfer.
        camera.reset_pipe()?;
    }
    ensure!(
        camera.vendor(0xbc, 0x18, 0, 1)?[0] == 0,
        "Duo replay engine did not return idle"
    );
    let retained = camera.vendor(0xbc, 0x23, 0, 1)?[0];
    ensure!(
        retained & !0x10 == 5,
        "Duo retained frame was lost: {retained:#x}"
    );
    camera.vendor(0xbd, 0x18, 1, 0)?;
    camera.phase("retained");
    Ok(())
}

pub fn raw_settings(s: &Settings, gain: i32, bin: u32) -> Result<Settings> {
    ensure!((1..=4).contains(&bin), "Duo bin must be 1..4");
    ensure!(
        s.width.is_multiple_of(8) && s.height.is_multiple_of(2),
        "Duo output dimensions require width multiple of 8 and even height"
    );
    let mut raw = s.clone();
    raw.width = s
        .width
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("Duo width overflow"))?;
    raw.height = s
        .height
        .checked_mul(bin)
        .ok_or_else(|| anyhow::anyhow!("Duo height overflow"))?;
    raw.x =
        s.x.checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("Duo x overflow"))?;
    raw.y =
        s.y.checked_mul(bin)
            .ok_or_else(|| anyhow::anyhow!("Duo y overflow"))?;
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
    let sensor = sensor_settings(&raw, info["productId"] == 0x260e);
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
fn sensor_settings(raw: &Settings, p25: bool) -> Settings {
    let mut sensor = raw.clone();
    // P25 64x64 DDR reads stall; 512x128 replay succeeds. Expand small
    // requests to at least 128 KiB, correct in sensor coordinates, then crop.
    if p25 && sensor.width * sensor.height < 65536 {
        sensor.width = sensor.width.max(512);
        sensor.height = sensor.height.max(128);
        sensor.x = sensor.x.min((6248 - sensor.width) / 16 * 16);
        sensor.y = sensor.y.min(4176 - sensor.height);
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
    let revision = match info["productId"].as_u64() {
        Some(0x2601) => Revision::Original,
        Some(0x260e) => Revision::P25,
        _ => anyhow::bail!("ASI2600 capture requires observed PID 2601 or 260e"),
    };
    ensure!(
        info["usbVersionBcd"] == 0x300,
        "ASI2600 capture requires USB3"
    );
    let started = Instant::now();
    // Cache identity while the handle is healthy. A failed USB read may make
    // even the serial query impossible until the handle has been replaced.
    let recovery_serial = if cfg!(windows) && revision == Revision::P25 {
        camera
            .vendor(0xc8, 0, 0, 8)
            .ok()
            .filter(|s| s.len() == 8 && s.iter().any(|&b| b != 0))
    } else {
        None
    };
    camera.phase("initializing");
    let result = (|| {
        let initialize = match revision {
            Revision::Original => asi2600_tables::INITIALIZE,
            Revision::P25 => asi2600_p25_tables::INITIALIZE,
        };
        for &(request, register, value) in initialize {
            // Host cooling/dew state must survive sensor initialization between frames.
            if camera.has_environment() && request == 0xbd && [0x19, 0x26].contains(&register) {
                continue;
            }
            camera.vendor(request, register, value, 0)?;
        }
        let defects = calibration(camera, s)?;
        writes(camera, asi2600_tables::RAW16)?;
        word(camera, 0xbd, 6, 45, 2)?;
        word(camera, 0xbd, 2, 24, 2)?;
        writes(camera, &[(0xb6, 0xa7, 1), (0xb6, 7, 1)])?;
        word(camera, 0xb6, 0xa8, s.x / 16, 2)?;
        word(camera, 0xb6, 8, s.y + 25, 2)?;
        word(camera, 0xbd, 0x40, s.width * s.height / 2, 4)?;
        camera.vendor(0xb6, 0x1d8, 4, 0)?;
        word(camera, 0xb6, 0xa, s.height, 2)?;
        word(camera, 0xb6, 0x1dd, s.width + 24, 2)?;
        word(camera, 0xbd, 8, s.height, 2)?;
        word(camera, 0xbd, 4, s.width, 2)?;
        // SDK bandwidth 40: original HMAX/divisor 779/3; P25 790/400.
        word(camera, 0xbd, 0x13, revision.hmax(), 2)?;
        word(
            camera,
            0xbd,
            0x24,
            if revision == Revision::P25 { 400 } else { 3 },
            2,
        )?;
        let (mode, analog, hcg, digital) = gain_registers(gain);
        camera.vendor(0xb6, 0x67f, mode, 0)?;
        word(camera, 0xb6, 0x30, analog, 2)?;
        word(camera, 0xb6, 0x32, analog, 2)?;
        writes(camera, &[(0xb6, 0x2f, hcg), (0xb6, 0x40, digital)])?;
        word(camera, 0xb6, 0x42, s.offset * 10, 2)?;
        word(camera, 0xb6, 0x44, s.offset * 10, 2)?;
        let (frame, shutter) = timing(s, revision);
        word(camera, 0xbd, 0x10, frame, 3)?;
        word(camera, 0xb6, 0x18, shutter, 2)?;
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
        ensure!(old == 1, "Duo old frame did not clear: {old}");
        let armed = Instant::now();
        camera.phase("exposing");
        writes(camera, &[(0xa9, 0, 0), (0xb6, 0x1ee, 1), (0xb6, 0, 5)])?;
        std::thread::sleep(Duration::from_millis(50));
        camera.vendor(0xb6, 0, 4, 0)?;
        let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
        camera.vendor(0xbd, 0, u16::from(flags & !0x10), 0)?;
        camera.reset_pipe()?;
        if s.microseconds >= 1_000_000 {
            // SDK 1.41 worker 14c30c..14c636: synchronize the FPGA, then
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
            ensure!(synchronized, "Duo long exposure synchronization failed");
            let f = camera.vendor(0xbc, 0xb, 0, 1)?[0];
            camera.vendor(0xbd, 0xb, u16::from(f | 1), 0)?;
            let exposure_ms = s.microseconds / 1000;
            if exposure_ms > 1000 {
                let trigger = Instant::now();
                let mut iteration = 0;
                loop {
                    match iteration {
                        6 => {
                            camera.vendor(0xb6, 0x1ee, 5, 0)?;
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
                    if elapsed >= u128::from(exposure_ms - 400) {
                        break;
                    }
                }
            } else {
                std::thread::sleep(Duration::from_millis(u64::from(exposure_ms - 205)));
            }
            let f = camera.vendor(0xbc, 0x19, 0, 1)?[0];
            camera.vendor(0xbd, 0x19, u16::from(f & !1), 0)?;
            camera.service_environment()?;
            std::thread::sleep(Duration::from_millis(100));
            camera.vendor(0xb6, 0x1ee, 1, 0)?;
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
                "Duo retained-frame timeout: {status}"
            );
            camera.service_environment()?;
            std::thread::sleep(Duration::from_millis(5));
        }
        // Sensor standby, deliberately without AA (which clears retained DDR).
        camera.phase("sensor_readout");
        // On this unit 23=15 can precede a safely freezable frame: immediate
        // standby repeatedly stalled 64x64 replay. The empirical 100ms guard
        // fixes that case. Long integrations must consume the initial pass
        // below; an extra settling delay does not substitute for that step.
        let settling = if revision == Revision::P25 {
            // BC23=15 starts readout; it does not mean every DDR row is fresh.
            // P25 full-frame offset transitions require one programmed sensor
            // frame interval before standby, plus the observed settling guard.
            Duration::from_micros(u64::from(frame) * u64::from(revision.hmax()) / 20 + 100_000)
        } else {
            Duration::from_millis(100)
        };
        let settle_started = Instant::now();
        while settle_started.elapsed() < settling {
            camera.service_environment()?;
            std::thread::sleep(Duration::from_millis(10));
        }
        writes(camera, &[(0xb6, 0x1ee, 5), (0xb6, 0, 5)])?;
        camera.phase("retained");
        // Short integrations stream; restart from frozen DDR. Long integrations
        // already schedule one pass: triggering replay first can cancel it.
        if s.microseconds < 1_000_000 {
            begin_retained_read(camera, revision)?;
        }
        let mut prefix = Vec::new();
        let mut read_errors = Vec::new();
        let mut continuity = crate::transfer::Continuity::default();
        let mut handle_reopens = 0;
        let mut data = loop {
            let attempt = (|| {
                if !read_errors.is_empty() {
                    // Try the sender/pipe first. Only escalate within the user's
                    // existing read retry budget, with evidence from this frame.
                    if read_errors.len() >= 2
                        && handle_reopens == 0
                        && continuity.has_pixels()
                        && let Some(serial) = &recovery_serial
                    {
                        handle_reopens += 1;
                        crate::diagnostics::log(
                            "warning",
                            "transfer.reopening",
                            "Reopening the camera handle before rereading the retained frame",
                        );
                        camera.reopen_same_camera(serial, Duration::from_secs(3))?;
                    }
                    begin_retained_read(camera, revision)?;
                }
                if read_errors.is_empty() && s.reopen_after_bytes > 0 {
                    prefix = camera.read_frame(s.reopen_after_bytes as usize)?;
                    // Leave the sender and sensor untouched: measure handle-close
                    // effects before the normal retained-sender restart sequence.
                    let before = camera.vendor(0xbc, 0x23, 0, 1)?[0];
                    camera.reopen_retained(Duration::from_millis(u64::from(s.reopen_delay_ms)))?;
                    let after = camera.vendor(0xbc, 0x23, 0, 1)?[0];
                    crate::diagnostics::details(
                        "info",
                        "research.reopened",
                        format_args!(
                            "USB handle reopened without initialization; retained status {before:#x} -> {after:#x}"
                        ),
                        json!({"before":before,"after":after,"prefixBytes":prefix.len(),"delayMs":s.reopen_delay_ms}),
                    );
                    anyhow::bail!("research USB handle reopened after {} bytes", prefix.len());
                }
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
                let data = camera.read_frame_checked(
                    s.width as usize * s.height as usize * 2,
                    5000,
                    &mut continuity,
                )?;
                frame_sequence(&data)?;
                Ok(data)
            })();
            match attempt {
                Ok(data) => break data,
                Err(error) => {
                    crate::diagnostics::read_failure(
                        "ASI2600MM Pro",
                        &error,
                        read_errors.len(),
                        s.read_retries,
                        true,
                    );
                    if read_errors.len() >= s.read_retries as usize
                        || error.is::<crate::transfer::ChangedFrame>()
                    {
                        return Err(error);
                    }
                    read_errors.push(error.to_string());
                }
            }
        };
        ensure!(
            prefix.is_empty() || data[4..prefix.len()] == prefix[4..],
            "Duo recovered pixels do not match interrupted prefix"
        );
        let sequence = frame_sequence(&data)?;
        camera.phase("validating");
        let wire_hash = format!("{:x}", Sha256::digest(&data));
        let wire_interior_hash = format!("{:x}", Sha256::digest(&data[4..data.len() - 4]));
        let replay_result = if replay {
            begin_retained_read(camera, revision)?;
            if s.replay_prefix_bytes > 0 {
                let prefix = camera.read_frame(s.replay_prefix_bytes as usize)?;
                ensure!(
                    prefix[4..] == data[4..prefix.len()],
                    "ASI2600 replay prefix changed"
                );
                begin_retained_read(camera, revision)?;
            }
            let mut errors = Vec::new();
            let second = loop {
                match camera.read_frame(data.len()).and_then(|frame| {
                    frame_sequence(&frame)?;
                    Ok(frame)
                }) {
                    Ok(frame) => break frame,
                    Err(error) if errors.len() < s.read_retries as usize => {
                        errors.push(error.to_string());
                        begin_retained_read(camera, revision)?;
                    }
                    Err(error) => return Err(error),
                }
            };
            let replay_sequence = frame_sequence(&second)?;
            ensure!(
                second[4..second.len() - 4] == data[4..data.len() - 4],
                "Duo retained pixels differ from original"
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
        let meta = json!({"model":if revision == Revision::P25 { "ASI2600MM Pro P25" } else { "ASI2600MM Pro" },"width":s.width,"height":s.height,"x":s.x,"y":s.y,
            "bin":1,"gain":gain,"offset":s.offset,"microseconds":s.microseconds,"bytes":data.len(),
            "wireSha256":wire_hash,"wireInteriorSha256":wire_interior_hash,
            "sha256":format!("{:x}",Sha256::digest(&data)),"sequence":sequence,
            "factoryDefects":defects.indices.len(),"acquisitionMs":armed.elapsed().as_millis(),"elapsedMs":started.elapsed().as_millis(),
            "replay":replay_result,"sdkLoaded":false,"readRecoveries":read_errors.len(),"readErrors":read_errors,
            "interruptedPrefixBytes":prefix.len(),"interruptedPrefixPixelsMatch":!prefix.is_empty(),
            "timeoutInjectionBytes":s.timeout_read_after_bytes,"reopenInjectionBytes":s.reopen_after_bytes});
        let mut meta = meta;
        meta["handleReopens"] = json!(handle_reopens);
        meta["retainedPixelsVerified"] = json!(continuity.verified_bytes() > 0);
        meta["verifiedRetainedBytes"] = json!(continuity.verified_bytes());
        Ok((meta, data))
    })();
    let cleanup = if s.keep_retained && result.is_ok() {
        crate::diagnostics::log(
            "info",
            "research.retained_on_exit",
            "Leaving validated frame in camera DDR for a separate verification worker",
        );
        Ok(())
    } else {
        stop(camera)
    };
    crate::completion::finish(result, cleanup)
}

/// Research-only proof of retention across process exit. Never delivers an
/// image to a frontend and never initializes the sensor or starts an exposure.
pub fn verify_retained(
    camera: &Camera,
    info: &Value,
    s: &Settings,
    expected: &str,
) -> Result<Value> {
    ensure!(
        info["productId"] == 0x260e && info["usbVersionBcd"] == 0x300,
        "retained verification is restricted to ASI2600 P25 USB3"
    );
    validate(s, 0)?;
    let started = Instant::now();
    let result = (|| {
        let mut errors = Vec::new();
        let data = loop {
            begin_retained_read(camera, Revision::P25)?;
            match camera
                .read_frame(s.width as usize * s.height as usize * 2)
                .and_then(|data| {
                    frame_sequence(&data)?;
                    Ok(data)
                }) {
                Ok(data) => break data,
                Err(error) if errors.len() < s.read_retries as usize => {
                    errors.push(format!("{error:#}"))
                }
                Err(error) => return Err(error),
            }
        };
        let actual = format!("{:x}", Sha256::digest(&data[4..data.len() - 4]));
        ensure!(
            actual == expected,
            "retained frame hash differs from the previous worker's frame"
        );
        Ok(
            json!({"pixelBytesIdentical":true,"wireInteriorSha256":actual,"bytes":data.len(),
            "readErrors":errors,"elapsedMs":started.elapsed().as_millis(),"sdkLoaded":false}),
        )
    })();
    let cleanup = stop(camera);
    let result = result?;
    cleanup?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p25_registers_match_sdk_141_camera_kit_observations() {
        // ASI2600 P25 kit e0a00a37..., not values derived from this implementation.
        for (height, microseconds, expected) in [
            (4176, 100_000, (4224, 846)),
            (256, 32, (304, 151)),
            (256, 100, (304, 150)),
            (256, 1_000, (304, 139)),
            (256, 10_000, (304, 25)),
            (256, 100_000, (2532, 1)),
            (256, 490_000, (12406, 1)),
            (256, 999_000, (25292, 1)),
            (256, 1_000_000, (431, 1)),
            (256, 1_001_000, (431, 1)),
            (256, 60_000_000, (431, 1)),
        ] {
            let s = Settings {
                height,
                microseconds,
                ..Settings::default()
            };
            assert_eq!(timing(&s, Revision::P25), expected);
        }
        for (gain, expected) in [
            (-25, (17, 0, 0, 0)),
            (99, (0, 2785, 0, 0)),
            (100, (0, 0, 1, 0)),
            (101, (0, 46, 1, 0)),
            (180, (0, 2464, 1, 0)),
            (350, (0, 3864, 1, 0)),
            (700, (0, 4030, 1, 64)),
        ] {
            assert_eq!(gain_registers(gain), expected);
        }
    }

    #[test]
    fn p25_small_transfers_cover_requested_roi_at_sensor_edges() {
        for (width, height) in [(64, 64), (64, 128), (512, 64), (6248, 64)] {
            for edge in [false, true] {
                let raw = Settings {
                    width,
                    height,
                    x: if edge {
                        (6248 - width) / 16 * 16
                    } else {
                        16.min((6248 - width) / 16 * 16)
                    },
                    y: if edge { 4176 - height } else { 2 },
                    ..Settings::default()
                };
                let sensor = sensor_settings(&raw, true);
                validate(&sensor, 100).unwrap();
                assert!(sensor.width * sensor.height * 2 >= 131072);
                assert!(sensor.x <= raw.x && sensor.y <= raw.y);
                assert!(sensor.x + sensor.width >= raw.x + raw.width);
                assert!(sensor.y + sensor.height >= raw.y + raw.height);
                let original = sensor_settings(&raw, false);
                assert_eq!(
                    (original.width, original.height, original.x, original.y),
                    (raw.width, raw.height, raw.x, raw.y)
                );
            }
        }
    }

    #[test]
    fn long_exposures_keep_host_timed_sensor_registers_and_sdk_range() {
        let mut settings = Settings {
            width: 6248,
            height: 4176,
            microseconds: 1_000_000,
            ..Settings::default()
        };
        let registers = timing(&settings, Revision::Original);
        for duration in [
            30_000_001,
            60_000_000,
            120_000_000,
            1_200_000_000,
            MAX_EXPOSURE_US,
        ] {
            settings.microseconds = duration;
            assert!(validate(&settings, 100).is_ok());
            assert_eq!(timing(&settings, Revision::Original), registers);
        }
        settings.microseconds = MAX_EXPOSURE_US + 1;
        assert!(validate(&settings, 100).is_err());
    }
}
