//! Experimental ASI676MC bin-1 RAW16 acquisition. No ASI DLL calls.
use crate::{asi676_tables, processing, protocol, settings::Settings, transport::Camera};
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

fn stop(camera: &Camera) -> Result<()> {
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

fn word(camera: &Camera, request: u8, register: u16, value: u32, bytes: u16) -> Result<()> {
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

fn calibration(camera: &Camera, settings: &Settings) -> Result<processing::Defects> {
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
        processing::Defects::decode(
            &data,
            settings.width as usize,
            settings.height as usize,
            settings.x as usize,
            settings.y as usize,
        )
    })();
    let restore = camera.vendor(0xbe, 1, 0, 0);
    let defects = result?;
    restore?;
    Ok(defects)
}

pub fn capture(
    camera: &Camera,
    info: &Value,
    settings: &Settings,
    replay: bool,
) -> Result<(Value, Vec<u8>)> {
    settings.validate()?;
    ensure!(
        info["productId"] == 0x676d && info["usbVersionBcd"] == 0x300,
        "direct acquisition is restricted to the observed ASI676MC USB3 device"
    );
    let start = Instant::now();
    let result = (|| -> Result<(Value, Vec<u8>)> {
        writes(camera, asi676_tables::INITIALIZE)?;
        let defects = calibration(camera, settings)?;
        writes(camera, asi676_tables::RAW16_FULL)?;
        // Set every requested value explicitly, regardless of the preceding owner.
        word(camera, 0xb6, 0x303c, settings.x, 2)?;
        word(camera, 0xb6, 0x3044, settings.y, 2)?;
        word(camera, 0xb6, 0x303e, settings.width, 2)?;
        word(camera, 0xb6, 0x3046, settings.height + 2, 2)?;
        word(camera, 0xbd, 0x40, settings.width * settings.height / 2, 4)?;
        word(camera, 0xbd, 8, settings.height, 2)?;
        word(camera, 0xbd, 4, settings.width, 2)?;
        let (hcg, gain) = settings.gain_registers();
        writes(
            camera,
            &[
                (0xb6, 0x3001, 1),
                (0xb6, 0x3030, hcg),
                (0xb6, 0x306c, gain & 255),
                (0xb6, 0x306d, gain >> 8),
                (0xb6, 0x3001, 0),
            ],
        )?;
        word(camera, 0xb6, 0x30dc, settings.offset, 2)?;
        let (frame_lines, shutter_lines) = settings.timing();
        word(camera, 0xbd, 0x10, frame_lines, 3)?;
        word(camera, 0xb6, 0x3050, shutter_lines, 3)?;
        let flags = camera.vendor(0xbc, 0, 0, 1)?[0];
        let flags = if settings.long_exposure() {
            flags | 0xc0
        } else {
            flags & !0xc0
        };
        camera.vendor(0xbd, 0, u16::from(flags), 0)?;
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
            std::thread::sleep(Duration::from_micros(u64::from(
                settings.microseconds - 200_000,
            )));
            let status = camera.vendor(0xbc, 0x19, 0, 1)?[0];
            camera.vendor(0xbd, 0x19, u16::from(status & !1), 0)?;
            let remaining = Duration::from_micros(u64::from(settings.microseconds))
                .saturating_sub(trigger.elapsed());
            std::thread::sleep(remaining);
            let flags = camera.vendor(0xbc, 0x0b, 0, 1)?[0];
            camera.vendor(0xbd, 0x0b, u16::from(flags | 1), 0)?;
            camera.vendor(0xbd, 0x0b, u16::from(flags & !1), 0)?;
        }
        let ready_wait = Instant::now();
        let ready_timeout =
            Duration::from_micros(u64::from(settings.microseconds)) + Duration::from_secs(5);
        let mut ready_samples = 0;
        loop {
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
                    crate::diagnostics::read_failure(
                        "ASI676MC",
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
            camera.vendor(0xbd, 0x18, 1, 0)?;
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
        protocol::replace_envelope(&mut data, settings.width as usize)?;
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
        let metadata = json!({"sdkLoaded":false,"model":"ASI676MC","width":settings.width,"height":settings.height,
            "x":settings.x,"y":settings.y,"gain":settings.gain,"offset":settings.offset,
            "exposureMicroseconds":settings.microseconds,"hostTimed":settings.long_exposure(),
            "bin":1,"format":"RAW16","bayer":"RGGB","defectCorrectionApplied":true,
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
    crate::completion::finish(result, cleanup)
}
