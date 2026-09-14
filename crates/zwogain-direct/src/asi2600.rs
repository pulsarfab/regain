//! Research-only ASI2600MM Duo acquisition over the installed Cypress driver.
use crate::{asi2600_tables, processing, settings::Settings, transport::Camera};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

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
        (32..=30_000_000).contains(&s.microseconds)
            && (-25..=700).contains(&gain)
            && s.offset <= 240
            && s.read_retries <= 5,
        "unsupported Duo exposure/controls"
    );
    let bytes = s.width * s.height * 2;
    for prefix in [s.interrupt_read_after_bytes, s.replay_prefix_bytes] {
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
pub fn timing(s: &Settings) -> (u32, u32) {
    let line = 779.0_f32 * 1000.0 / 20000.0;
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
fn begin_retained_read(camera: &Camera) -> Result<()> {
    camera.reset_pipe()?;
    camera.vendor(0xbd, 0x18, 0, 0)?;
    std::thread::sleep(Duration::from_millis(100));
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
    let (mut meta, mut data) = capture_native(camera, info, &raw, gain, replay)?;
    if bin > 1 {
        data =
            processing::bin_average(&data, raw.width as usize, raw.height as usize, bin as usize)?;
    }
    meta["wireBytes"] = json!(raw.width * raw.height * 2);
    meta["rawWidth"] = json!(raw.width);
    meta["rawHeight"] = json!(raw.height);
    meta["width"] = json!(s.width);
    meta["height"] = json!(s.height);
    meta["bin"] = json!(bin);
    meta["x"] = json!(s.x);
    meta["y"] = json!(s.y);
    meta["bytes"] = json!(data.len());
    meta["sha256"] = json!(format!("{:x}", Sha256::digest(&data)));
    Ok((meta, data))
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
        info["productId"] == 0x2601 && info["usbVersionBcd"] == 0x300,
        "Duo research capture requires observed PID 2601 USB3"
    );
    let started = Instant::now();
    let result = (|| {
        writes(camera, asi2600_tables::INITIALIZE)?;
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
        // Traced bandwidth 40: HMAX 779 and FPGA auxiliary divisor 3.
        word(camera, 0xbd, 0x13, 779, 2)?;
        word(camera, 0xbd, 0x24, 3, 2)?;
        let (mode, analog, hcg, digital) = gain_registers(gain);
        camera.vendor(0xb6, 0x67f, mode, 0)?;
        word(camera, 0xb6, 0x30, analog, 2)?;
        word(camera, 0xb6, 0x32, analog, 2)?;
        writes(camera, &[(0xb6, 0x2f, hcg), (0xb6, 0x40, digital)])?;
        word(camera, 0xb6, 0x42, s.offset * 10, 2)?;
        word(camera, 0xb6, 0x44, s.offset * 10, 2)?;
        let (frame, shutter) = timing(s);
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
            std::thread::sleep(Duration::from_millis(100));
            camera.vendor(0xb6, 0x1ee, 1, 0)?;
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
            std::thread::sleep(Duration::from_millis(5));
        }
        // Sensor standby, deliberately without AA (which clears retained DDR).
        // On this unit 23=15 can precede a safely freezable frame: immediate
        // standby repeatedly stalled 64x64 replay. The empirical 100ms guard
        // fixes that case. Long integrations must consume the initial pass
        // below; an extra settling delay does not substitute for that step.
        std::thread::sleep(Duration::from_millis(100));
        writes(camera, &[(0xb6, 0x1ee, 5), (0xb6, 0, 5)])?;
        // Short integrations stream; restart from frozen DDR. Long integrations
        // already schedule one pass: triggering replay first can cancel it.
        if s.microseconds < 1_000_000 {
            begin_retained_read(camera)?;
        }
        let mut prefix = Vec::new();
        let mut read_errors = Vec::new();
        let mut data = loop {
            let attempt = (|| {
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
            "Duo recovered pixels do not match interrupted prefix"
        );
        let sequence = frame_sequence(&data)?;
        let wire_hash = format!("{:x}", Sha256::digest(&data));
        let replay_result = if replay {
            begin_retained_read(camera)?;
            if s.replay_prefix_bytes > 0 {
                camera.read_frame(s.replay_prefix_bytes as usize)?;
                begin_retained_read(camera)?;
            }
            let second = camera.read_frame(data.len())?;
            let replay_sequence = frame_sequence(&second)?;
            ensure!(
                second[4..second.len() - 4] == data[4..data.len() - 4],
                "Duo retained pixels differ from original"
            );
            json!({"pixelBytesIdentical":true,"wireBytesIdentical":second==data,"bytes":data.len(),
                "firstSequence":sequence,"replaySequence":replay_sequence})
        } else {
            Value::Null
        };
        let last = data.len() - 4;
        let row = s.width as usize * 2;
        data.copy_within(row..row + 4, 0);
        data.copy_within(last - row..last - row + 4, last);
        defects.correct(&mut data)?;
        let meta = json!({"model":"ASI2600MM Duo","width":s.width,"height":s.height,"x":s.x,"y":s.y,
            "bin":1,"gain":gain,"offset":s.offset,"microseconds":s.microseconds,"bytes":data.len(),
            "wireSha256":wire_hash,"sha256":format!("{:x}",Sha256::digest(&data)),"sequence":sequence,
            "factoryDefects":defects.indices.len(),"acquisitionMs":armed.elapsed().as_millis(),"elapsedMs":started.elapsed().as_millis(),
            "replay":replay_result,"sdkLoaded":false,"readRecoveries":read_errors.len(),"readErrors":read_errors,
            "interruptedPrefixBytes":prefix.len(),"interruptedPrefixPixelsMatch":!prefix.is_empty()});
        Ok((meta, data))
    })();
    let cleanup = stop(camera);
    let frame = result?;
    cleanup?;
    Ok(frame)
}
