//! Shared, trace-verified ASI585MM Pro/ASI662MC/ASI676MC sensor acquisition. No ASI DLL calls.
use crate::asi::direct::{processing, protocol, settings::Settings, transport::Camera};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

/// Sensor-specific constants; lifecycle and retained-frame recovery are shared.
pub struct Profile {
    pub name: &'static str,
    pub pid: u16,
    pub width: u32,
    pub height: u32,
    pub offset_max: u32,
    pub alignment: u32,
    pub y_alignment: u32,
    pub sensor_height_alignment: u32,
    pub color: bool,
    pub sensor_alignment: u32,
    pub hmax: u16,
    pub gain_register: u16,
    pub hcg_threshold: u32,
    pub hcg_offset: u32,
    pub minimum_frame_lines: u32,
    pub initialize: &'static [(u8, u16, u16)],
    pub raw16: &'static [(u8, u16, u16)],
}
impl Profile {
    pub fn validate(&self, settings: &Settings) -> Result<()> {
        settings.validate_bayer(self.width, self.height, self.offset_max)?;
        ensure!(
            settings.x.is_multiple_of(self.alignment)
                && settings.y.is_multiple_of(self.y_alignment),
            "{} ROI origin must be aligned to x={} and y={} pixels",
            self.name,
            self.alignment,
            self.y_alignment
        );
        Ok(())
    }
    pub fn replace_envelope(&self, data: &mut [u8], width: usize) -> Result<()> {
        if self.color {
            return protocol::replace_envelope(data, width);
        }
        ensure!(
            width >= 8 && data.len() >= width * 6 && data.len().is_multiple_of(width * 2),
            "invalid mono geometry"
        );
        let last = data.len() - 4;
        data.copy_within(width * 2..width * 2 + 4, 0);
        data.copy_within(last - width * 2..last - width * 2 + 4, last);
        Ok(())
    }
    pub fn gain(&self, gain: u32) -> (u16, u16) {
        if gain >= self.hcg_threshold {
            (1, ((gain - self.hcg_offset) / 3) as u16)
        } else {
            (0, (gain / 3) as u16)
        }
    }
    pub fn timing(&self, settings: &Settings) -> (u32, u32) {
        let (frame, shutter) = settings.timing_with_hmax(self.hmax);
        // ASI662 short full-frame readout can outrun the retained buffer.
        // Lengthen the frame interval and shutter delay equally, preserving
        // integration lines. Host-timed long exposures keep their SDK timing.
        let padding = if settings.long_exposure() {
            0
        } else {
            self.minimum_frame_lines.saturating_sub(frame)
        };
        (frame + padding, shutter + padding)
    }

    pub(super) fn timing_for_link(
        &self,
        settings: &Settings,
        link: super::link::Link,
    ) -> (u32, u32) {
        let (frame, shutter) = self.timing(settings);
        if link != super::link::Link::HighSpeed || settings.long_exposure() {
            return (frame, shutter);
        }
        // USB 2 ASI676 short-ROI testing exposed a rollover between the first
        // read and retained replay. Apply the same ~100 ms pacing strategy as
        // ASI662: extend frame AND shutter delay, never integration duration.
        let paced = Settings {
            microseconds: 100_000,
            ..settings.clone()
        };
        let minimum = paced.timing_with_hmax(self.hmax).0;
        let padding = minimum.saturating_sub(frame);
        (frame + padding, shutter + padding)
    }
}

fn writes(camera: &Camera, commands: &[(u8, u16, u16)]) -> Result<()> {
    for &(request, register, value) in commands {
        camera.vendor(request, register, value, 0)?;
    }
    Ok(())
}

pub(super) fn set_gain(camera: &Camera, profile: &Profile, gain: u32) -> Result<()> {
    let (hcg, gain) = profile.gain(gain);
    writes(
        camera,
        &[
            (0xb6, 0x3001, 1),
            (0xb6, 0x3030, hcg),
            (0xb6, profile.gain_register, gain & 255),
            (0xb6, profile.gain_register + 1, gain >> 8),
            (0xb6, 0x3001, 0),
        ],
    )
}

pub(super) fn exposure_writes(
    profile: &Profile,
    settings: &Settings,
    link: super::link::Link,
    flags: u8,
) -> Vec<(u8, u16, u16)> {
    let mut commands = Vec::new();
    let mut flags = flags;
    // Preserve unrelated mode bits. SDK enters bit 6 then 7; exits 7 then 6.
    for mask in if settings.long_exposure() {
        [0x40, 0x80]
    } else {
        [0x80, 0x40]
    } {
        let next = if settings.long_exposure() {
            flags | mask
        } else {
            flags & !mask
        };
        if next != flags {
            commands.push((0xbd, 0, u16::from(next)));
            flags = next;
        }
    }
    let (frame, shutter) = profile.timing_for_link(settings, link);
    for (request, register, hold, value) in
        [(0xbd, 0x10, 1, frame), (0xb6, 0x3050, 0x3001, shutter)]
    {
        commands.push((request, hold, 1));
        for byte in 0..3 {
            commands.push((
                request,
                register + byte,
                ((value >> (byte * 8)) & 255) as u16,
            ));
        }
        commands.push((request, hold, 0));
    }
    commands
}

pub(super) fn set_exposure(
    camera: &Camera,
    profile: &Profile,
    settings: &Settings,
    link: super::link::Link,
) -> Result<()> {
    let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
    writes(camera, &exposure_writes(profile, settings, link, flags))
}

#[test]
fn scalar_exposure_plan_matches_sdk_order_without_stream_commands() {
    use super::{asi662, asi676, link::Link};
    for profile in [&asi662::PROFILE, &asi676::PROFILE] {
        let mut settings = Settings {
            microseconds: 1_000_000,
            ..Settings::default()
        };
        let enter = exposure_writes(profile, &settings, Link::HighSpeed, 0x21);
        assert_eq!(
            &enter[..3],
            &[(0xbd, 0, 0x61), (0xbd, 0, 0xe1), (0xbd, 1, 1)]
        );
        settings.microseconds = 234_000;
        let exit = exposure_writes(profile, &settings, Link::HighSpeed, 0xe1);
        assert_eq!(
            &exit[..3],
            &[(0xbd, 0, 0x61), (0xbd, 0, 0x21), (0xbd, 1, 1)]
        );
        for plan in [enter, exit] {
            assert_eq!(plan.len(), 12);
            assert_eq!(plan[6], (0xbd, 1, 0));
            assert_eq!(plan[7], (0xb6, 0x3001, 1));
            assert_eq!(plan[11], (0xb6, 0x3001, 0));
            assert!(!plan.iter().any(
                |&(request, register, _)| matches!(request, 0xa9 | 0xaa) || register == 0x3000
            ));
        }
    }
}

pub(super) fn stop(camera: &Camera) -> Result<()> {
    let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
    writes(
        camera,
        &[
            (0xbd, 0, u16::from(flags | 0x10)),
            (0xb6, 0x3000, 1),
            (0xaa, 0, 0),
        ],
    )?;
    camera.reset_pipe()
}

pub(super) fn word(
    camera: &Camera,
    request: u8,
    register: u16,
    value: u32,
    bytes: u16,
) -> Result<()> {
    let hold = if request == 0xb6 { 0x3001 } else { 1 };
    camera.vendor(request, hold, 1, 0)?;
    for byte in 0..bytes {
        camera.vendor(
            request,
            register + byte,
            ((value >> (byte * 8)) & 255) as u16,
            0,
        )?;
    }
    camera.vendor(request, hold, 0, 0)?;
    Ok(())
}

fn restart_retained_read(camera: &Camera) -> Result<()> {
    // Every previous request has reached terminal completion before this call.
    // Explicitly drop the active replay bit before raising it again: simply
    // writing 1 while a partial replay is active did not restart the stream.
    camera.reset_pipe()?;
    camera.vendor(0xbd, 0x18, 0, 0)?;
    std::thread::sleep(Duration::from_millis(100));
    ensure!(
        camera.vendor(0xbc, 0x18, 0, 1)?[0] == 0,
        "replay engine did not return idle"
    );
    ensure!(
        camera.vendor(0xbc, 0x23, 0, 1)?[0] == 5,
        "camera no longer retains the frame"
    );
    camera.vendor(0xbd, 0x18, 1, 0)?;
    Ok(())
}

fn calibration(
    camera: &Camera,
    settings: &Settings,
    profile: &Profile,
) -> Result<processing::Defects> {
    // BE selects EEPROM access; C3 is strictly IN. Never write calibration data.
    camera.vendor(0xbe, 0, 0, 0)?;
    let result = (|| -> Result<processing::Defects> {
        let mut data = camera.vendor(0xc3, 0, 0x400, 2048)?;
        let length = processing::declared_length(&data)?;
        while data.len() < length {
            let offset = data.len();
            let amount = (length - offset).min(2048).next_multiple_of(256);
            data.extend(camera.vendor(0xc3, 0, ((0x40000 + offset) >> 8) as u16, amount as u16)?);
        }
        processing::Defects::decode_profile(
            &data,
            (
                settings.width as usize,
                settings.height as usize,
                settings.x as usize,
                settings.y as usize,
            ),
            (
                profile.width as usize,
                profile.height as usize,
                if profile.color { 2 } else { 1 },
                12,
            ),
        )
    })();
    let restore = camera.vendor(0xbe, 1, 0, 0);
    let defects = result?;
    restore?;
    Ok(defects)
}

pub(super) fn configure(
    camera: &Camera,
    info: &Value,
    settings: &Settings,
    profile: &Profile,
) -> Result<(processing::Defects, u32, u32)> {
    profile.validate(settings)?;
    let link = super::link::validate(info, u32::from(profile.pid))?;
    writes(camera, profile.initialize)?;
    let defects = calibration(camera, settings, profile)?;
    writes(camera, profile.raw16)?;
    // Set every requested value explicitly, regardless of the preceding owner.
    word(camera, 0xb6, 0x303c, settings.x, 2)?;
    word(camera, 0xb6, 0x3044, settings.y, 2)?;
    word(
        camera,
        0xb6,
        0x303e,
        settings.width.next_multiple_of(profile.sensor_alignment),
        2,
    )?;
    word(
        camera,
        0xb6,
        0x3046,
        settings
            .height
            .next_multiple_of(profile.sensor_height_alignment)
            + 2,
        2,
    )?;
    word(camera, 0xbd, 0x40, settings.width * settings.height / 2, 4)?;
    word(camera, 0xbd, 8, settings.height, 2)?;
    word(camera, 0xbd, 4, settings.width, 2)?;
    let (hcg, gain) = profile.gain(settings.gain);
    writes(
        camera,
        &[
            (0xb6, 0x3001, 1),
            (0xb6, 0x3030, hcg),
            (0xb6, profile.gain_register, gain & 255),
            (0xb6, profile.gain_register + 1, gain >> 8),
            (0xb6, 0x3001, 0),
        ],
    )?;
    word(camera, 0xb6, 0x30dc, settings.offset, 2)?;
    let (frame_lines, shutter_lines) = profile.timing_for_link(settings, link);
    set_exposure(camera, profile, settings, link)?;
    Ok((defects, frame_lines, shutter_lines))
}

pub fn capture(
    camera: &Camera,
    info: &Value,
    settings: &Settings,
    replay: bool,
    profile: &Profile,
) -> Result<(Value, Vec<u8>)> {
    profile.validate(settings)?;
    super::link::validate(info, u32::from(profile.pid))?;
    let start = Instant::now();
    let result = (|| -> Result<(Value, Vec<u8>)> {
        let (defects, frame_lines, shutter_lines) = configure(camera, info, settings, profile)?;
        stop(camera)?;
        let status_before_arm = camera.vendor(0xbc, 0x23, 0, 1)?[0];
        ensure!(
            status_before_arm == 1,
            "old retained-frame state did not clear before exposure"
        );
        let armed = Instant::now();
        writes(
            camera,
            &[(0xa9, 0, 0), (0xb6, 0x3004, 0), (0xb6, 0x3000, 0)],
        )?;
        std::thread::sleep(Duration::from_millis(50));
        let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
        writes(camera, &[(0xbd, 0, u16::from(flags & !0x10))])?;
        camera.reset_pipe()?;
        let status_after_arm = camera.vendor(0xbc, 0x23, 0, 1)?[0];
        if settings.long_exposure() {
            std::thread::sleep(Duration::from_millis(30));
            let flags = camera.vendor(0xbc, 0x0b, 0, 1)?[0];
            camera.vendor(0xbd, 0x0b, u16::from(flags | 1), 0)?;
            let trigger = Instant::now();
            super::bayer_video::wait_until_servicing(
                trigger,
                Duration::from_micros(u64::from(settings.microseconds - 200_000)),
                &std::sync::atomic::AtomicBool::new(false),
                || camera.service_environment(),
            )?;
            let status = camera.vendor(0xbc, 0x19, 0, 1)?[0];
            camera.vendor(0xbd, 0x19, u16::from(status & !1), 0)?;
            let remaining = Duration::from_micros(u64::from(settings.microseconds))
                .saturating_sub(trigger.elapsed());
            super::bayer_video::wait_until_servicing(
                Instant::now(),
                remaining,
                &std::sync::atomic::AtomicBool::new(false),
                || camera.service_environment(),
            )?;
            let flags = camera.vendor(0xbc, 0x0b, 0, 1)?[0];
            camera.vendor(0xbd, 0x0b, u16::from(flags | 1), 0)?;
            camera.vendor(0xbd, 0x0b, u16::from(flags & !1), 0)?;
        }
        let ready_wait = Instant::now();
        let ready_timeout =
            Duration::from_micros(u64::from(settings.microseconds)) + Duration::from_secs(5);
        let mut ready_samples = 0;
        loop {
            camera.service_environment()?;
            let status = camera.vendor(0xbc, 0x23, 0, 1)?[0];
            ready_samples += 1;
            if status == 5 {
                break;
            }
            ensure!(
                ready_wait.elapsed() < ready_timeout,
                "camera did not report a retained frame; status {status}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        // Freeze the sensor before consuming the retained buffer. Full stop AA
        // clears the firmware's retained-frame flag; sensor standby preserves it.
        camera.vendor(0xb6, 0x3000, 1, 0)?;
        let length = settings.width as usize * settings.height as usize * 2;
        let mut read_errors = Vec::new();
        let mut interrupted_prefix = Vec::new();
        let mut data = loop {
            let read = (|| -> Result<Vec<u8>> {
                if read_errors.is_empty() && settings.interrupt_read_after_bytes > 0 {
                    interrupted_prefix =
                        camera.read_frame(settings.interrupt_read_after_bytes as usize)?;
                    anyhow::bail!(
                        "injected host read interruption after {} bytes (not a USB error)",
                        settings.interrupt_read_after_bytes
                    );
                }
                let data = camera.read_frame(length)?;
                protocol::frame_sequence(&data, length)?;
                Ok(data)
            })();
            match read {
                Ok(data) => break data,
                Err(error) => {
                    crate::asi::direct::diagnostics::read_failure(
                        profile.name,
                        &error,
                        read_errors.len(),
                        settings.read_retries,
                        true,
                    );
                    if read_errors.len() >= settings.read_retries as usize {
                        return Err(error);
                    }
                    read_errors.push(error.to_string());
                    restart_retained_read(camera)?;
                }
            }
        };
        ensure!(
            data.starts_with(&interrupted_prefix),
            "recovered frame does not match the interrupted prefix"
        );
        let acquisition_ms = armed.elapsed().as_millis();
        let sequence = protocol::frame_sequence(
            &data,
            settings.width as usize * settings.height as usize * 2,
        )?;
        let mut replay_result = Value::Null;
        if replay {
            let mut interrupted_state = Value::Null;
            let status = camera.vendor(0xbc, 0x23, 0, 1)?[0];
            ensure!(status == 5, "unrecognized retained-frame status {status}");
            camera.reset_pipe()?;
            let state = camera.vendor(0xbc, 0x18, 0, 1)?[0];
            ensure!(state == 0, "replay engine is not idle: {state}");
            restart_retained_read(camera)?;
            if settings.replay_prefix_bytes > 0 {
                // Deliberately abandon a partially consumed replay. This is a
                // host interruption, not a fabricated USB error or bus fault.
                camera.read_frame(settings.replay_prefix_bytes as usize)?;
                let partial_state = camera.vendor(0xbc, 0x18, 0, 1)?[0];
                interrupted_state = json!(partial_state);
                restart_retained_read(camera)?;
            }
            let replayed = camera.read_frame(data.len())?;
            protocol::frame_sequence(&replayed, data.len())?;
            ensure!(
                replayed == data,
                "retained-frame replay differs from original frame"
            );
            replay_result = json!({"status23":status,"state18":state,"bytes":replayed.len(),
                "allBytesIdentical":true,"additionalExposures":0,
                "discardedPrefixBytes":settings.replay_prefix_bytes,"injectedUsbError":false});
            replay_result["interruptedState18"] = interrupted_state;
        }
        let wire_digest = format!("{:x}", Sha256::digest(&data));
        let boundary_words = [
            format!("{:08x}", u32::from_le_bytes(data[..4].try_into().unwrap())),
            format!(
                "{:08x}",
                u32::from_le_bytes(data[data.len() - 4..].try_into().unwrap())
            ),
        ];
        profile.replace_envelope(&mut data, settings.width as usize)?;
        defects.correct(&mut data)?;
        let mut sum = 0_u64;
        let mut min = u16::MAX;
        let mut max = 0;
        let mut nonzero = 0;
        for bytes in data.as_chunks::<2>().0 {
            let pixel = u16::from_le_bytes([bytes[0], bytes[1]]);
            sum += u64::from(pixel);
            min = min.min(pixel);
            max = max.max(pixel);
            nonzero += usize::from(pixel != 0);
        }
        let metadata = json!({"sdkLoaded":false,"model":profile.name,"width":settings.width,"height":settings.height,
            "x":settings.x,"y":settings.y,"gain":settings.gain,"offset":settings.offset,
            "exposureMicroseconds":settings.microseconds,"hostTimed":settings.long_exposure(),
            "bin":1,"format":"RAW16","bayer":if profile.color { json!("RGGB") } else { Value::Null },"defectCorrectionApplied":true,
            "defectCount":defects.indices.len(),"defectIndexSha256":defects.index_hash(),
            "transportPixelsReplaced":true,"wireSha256":wire_digest,"wireBoundaryWords":boundary_words,
            "sha256":format!("{:x}",Sha256::digest(&data)),"acquisitionMs":acquisition_ms,
            "boundarySequence":sequence,"replay":replay_result,
            "retentionStatusBeforeArm":status_before_arm,"retentionStatusAfterArm":status_after_arm,
            "retentionPolls":ready_samples,"sensorFrozenBeforeRead":true,
            "readoutRetriesUsed":read_errors.len(),"readoutErrors":read_errors,
            "injectedHostInterruptionBytes":settings.interrupt_read_after_bytes,
            "interruptedPrefixMatches":!interrupted_prefix.is_empty(),
            "frameLines":frame_lines,"shutterLines":shutter_lines,
            "bytes":data.len(),"nonzeroPixels":nonzero,"minimum":min,"maximum":max,
            "mean":sum as f64 / (data.len()/2) as f64,"elapsedMs":start.elapsed().as_millis()});
        Ok((metadata, data))
    })();
    let cleanup = stop(camera);
    crate::asi::direct::completion::finish(result, cleanup)
}

#[test]
fn usb2_pacing_preserves_integration_and_leaves_usb3_and_long_exposures_unchanged() {
    use super::link::Link;
    for profile in [&super::asi662::PROFILE, &super::asi676::PROFILE] {
        for height in [64, 482, profile.height] {
            for microseconds in [32, 1000, 100000, 999999, 1000000, 30000000, 2000000000] {
                let settings = Settings {
                    height,
                    microseconds,
                    ..Settings::default()
                };
                let original = profile.timing(&settings);
                assert_eq!(
                    profile.timing_for_link(&settings, Link::SuperSpeed),
                    original
                );
                let paced = profile.timing_for_link(&settings, Link::HighSpeed);
                assert_eq!(paced.0 - paced.1, original.0 - original.1);
                if settings.long_exposure() {
                    assert_eq!(paced, original);
                } else {
                    assert!(f64::from(paced.0) * f64::from(profile.hmax) / 20.0 >= 100000.0);
                }
            }
        }
    }
}
