//! Isolated experimental direct backend; SDK remains the plugin's default.
mod asi220;
mod asi220_tables;
mod asi2600;
mod asi2600_p25_tables;
mod asi2600_tables;
mod asi585;
mod asi585_tables;
mod asi6200;
mod asi6200_tables;
mod asi662;
mod asi662_tables;
mod asi676;
mod asi676_tables;
mod bayer;
mod bayer_video;
mod completion;
mod diagnostics;
mod environment;
mod link;
mod processing;
mod protocol;
mod server;
mod settings;
mod transfer;
mod transport;
use anyhow::{Result, ensure};

fn probe_product(args: &[String]) -> Result<Option<u16>> {
    if args.first().is_none_or(|arg| arg != "--probe-pid") {
        return Ok(None);
    }
    ensure!(args.len() == 2, "Usage: --probe-pid HEX_PRODUCT_ID");
    let pid = u16::from_str_radix(args[1].trim_start_matches("0x"), 16)?;
    ensure!(
        matches!(
            pid,
            0x585e | 0x662b | 0x676d | 0x2601 | 0x260e | 0x620b | 0x2209
        ),
        "descriptor probe requires a supported camera product ID"
    );
    Ok(Some(pid))
}

#[cfg(windows)]
fn research_port_operation(cycle: bool) -> Result<()> {
    use std::time::{Duration, Instant};
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(30));
        std::process::exit(124);
    });
    let paths: Vec<_> = transport::enumerate()?
        .into_iter()
        .filter(|p| p.matches(0x03c3, 0x260e))
        .collect();
    ensure!(
        paths.len() == 1,
        "port experiment requires exactly one ASI2600 P25"
    );
    let camera = transport::Camera::open(&paths[0])?;
    let info = camera.probe()?;
    ensure!(
        info["productId"] == 0x260e
            && info["usbVersionBcd"] == 0x300
            && info["driverVersionRaw"] == "0x01020200",
        "port experiment requires the inspected P25 USB3 driver"
    );
    ensure!(
        camera.vendor(0xbc, 0x19, 0, 1)?[0] & 0x80 != 0,
        "disable cooling before this port experiment"
    );
    let serial = camera.vendor(0xc8, 0, 0, 8)?;
    ensure!(
        serial.iter().any(|&b| b != 0),
        "camera identity unavailable"
    );
    let before = camera.vendor(0xbc, 0x23, 0, 1)?[0];
    let started = Instant::now();
    camera.research_port_operation(cycle)?;
    camera.reopen_same_camera(&serial, Duration::from_secs(3))?;
    let after = camera.vendor(0xbc, 0x23, 0, 1)?[0];
    println!(
        "{}",
        serde_json::json!({"operation":if cycle {"cycle-port"} else {"reset-port"},
        "beforeRetainedStatus":before,"afterRetainedStatus":after,"identityVerified":true,
        "elapsedMs":started.elapsed().as_millis(),"device":camera.probe()?})
    );
    Ok(())
}

pub fn run(args: Vec<String>) -> Result<()> {
    transport::require_sdk_absent()?;
    let probe_product = probe_product(&args)?;
    if args.first().is_some_and(|a| a == "--usb-target") {
        ensure!(
            (3..=4).contains(&args.len()),
            "Usage: --usb-target CAMERA_NAME SERIAL [LOCATOR]"
        );
        return server::usb_target(&args[1], &args[2], args.get(3).map(String::as_str));
    }
    #[cfg(windows)]
    if args == ["--reset-port-2600-p25"] || args == ["--cycle-port-2600-p25"] {
        return research_port_operation(args[0].starts_with("--cycle"));
    }
    if args == ["--serve"] || args == ["--serve", "--simulate"] {
        return server::run(args.len() == 2);
    }
    if args == ["--process-frame"] {
        return processing::process_stream();
    }
    let asi585 = args.first().is_some_and(|a| a == "--capture-585");
    let asi662 = args.first().is_some_and(|a| a == "--capture-662");
    let video_profile = if asi585 {
        &asi585::PROFILE
    } else if asi662 {
        &asi662::PROFILE
    } else {
        &asi676::PROFILE
    };
    let asi6200 = args.first().is_some_and(|a| a == "--capture-6200");
    let verify_retained = args
        .first()
        .is_some_and(|a| a == "--verify-retained-2600-p25");
    let p25 = verify_retained || args.first().is_some_and(|a| a == "--capture-2600-p25");
    let duo = p25 || args.first().is_some_and(|a| a == "--capture-duo");
    let guide = args.first().is_some_and(|a| a == "--capture-guide");
    let capture = asi585
        || asi662
        || asi6200
        || duo
        || guide
        || args.first().is_some_and(|a| a == "--capture");
    let mut settings = settings::Settings::default();
    let mut duo_gain = 0_i32;
    let mut duo_bin = 1_u32;
    if duo {
        settings.width = 6248;
        settings.height = 4176;
    }
    if asi6200 {
        settings.width = 9576;
        settings.height = 6388;
    }
    if guide {
        settings.width = 1920;
        settings.height = 1080;
        settings.offset = 200;
    }
    if asi662 {
        settings.width = 1920;
        settings.height = 1080;
        settings.offset = 15;
    }
    if asi585 {
        settings.width = 3840;
        settings.height = 2160;
        settings.offset = 3;
    }
    let mut frames = 1_u32;
    let mut stream = false;
    let mut video = false;
    let mut fps_specified = false;
    let mut retries_specified = false;
    let mut replay = false;
    let mut expected_wire_hash = None;
    if capture {
        let mut options = args[1..].iter();
        while let Some(option) = options.next() {
            if option == "--max-fps" {
                fps_specified = true;
                settings.video_max_fps = options
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing max FPS"))?
                    .parse()?;
                continue;
            }
            if option == "--video" {
                ensure!(
                    asi585 || asi662 || args[0] == "--capture",
                    "video is only available for ASI585MM Pro/ASI662MC/ASI676MC"
                );
                video = true;
                continue;
            }
            if option == "--keep-retained" && p25 && !verify_retained {
                settings.keep_retained = true;
                continue;
            }
            if option == "--expected-wire-sha256" && verify_retained {
                let hash = options
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing expected hash"))?;
                ensure!(
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                    "invalid expected hash"
                );
                expected_wire_hash = Some(hash.to_ascii_lowercase());
                continue;
            }
            if option == "--replay" {
                replay = true;
                continue;
            }
            if option == "--stream" {
                stream = true;
                continue;
            }
            if option == "--transfer-timeout-seconds" {
                settings.transfer_timeout_seconds = options
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing transfer timeout"))?
                    .parse()?;
                continue;
            }
            if (duo || asi6200) && option == "--gain" {
                duo_gain = options
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("missing gain"))?
                    .parse()?;
                continue;
            }
            let value: u32 = options
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing option value"))?
                .parse()?;
            match option.as_str() {
                "--bin" if duo || guide || asi6200 || asi585 => duo_bin = value,
                "--width" => settings.width = value,
                "--height" => settings.height = value,
                "--x" => settings.x = value,
                "--y" => settings.y = value,
                "--microseconds" => settings.microseconds = value,
                "--gain" => settings.gain = value,
                "--offset" => settings.offset = value,
                "--frames" => frames = value,
                "--read-chunk-kib" => settings.read_chunk_kib = value,
                "--replay-prefix-bytes" => {
                    settings.replay_prefix_bytes = value;
                    replay = true;
                }
                "--interrupt-read-after-bytes" => settings.interrupt_read_after_bytes = value,
                "--timeout-read-after-bytes" if duo || asi6200 => {
                    settings.timeout_read_after_bytes = value
                }
                "--read-retries" => {
                    retries_specified = true;
                    settings.read_retries = value;
                }
                "--reopen-after-bytes" if duo => settings.reopen_after_bytes = value,
                "--reopen-delay-ms" if duo => settings.reopen_delay_ms = value,
                _ => anyhow::bail!("unknown capture option {option}"),
            }
        }
        if asi6200 {
            asi6200::raw_settings(&settings, duo_gain, duo_bin)?;
        } else if duo {
            asi2600::raw_settings(&settings, duo_gain, duo_bin)?;
        } else if guide {
            asi220::raw_settings(&settings, duo_bin)?;
            ensure!(!replay, "guide retained replay is not established");
        } else if asi585 {
            asi585::raw_settings(&settings, duo_bin)?;
        } else if asi662 {
            asi662::PROFILE.validate(&settings)?;
        } else {
            settings.validate()?;
        }
        ensure!(
            (1..=if video { 1000 } else { 20 }).contains(&frames),
            "invalid frame count"
        );
        ensure!(!fps_specified || video, "--max-fps requires --video");
        if video {
            bayer_video::validate(
                &if asi585 {
                    asi585::raw_settings(&settings, duo_bin)?
                } else {
                    settings.clone()
                },
                video_profile,
            )?;
            ensure!(!replay, "video does not support retained-frame replay");
            ensure!(
                !retries_specified || settings.read_retries == 0,
                "video does not support retained read retries"
            );
            settings.read_retries = 0;
        }
        ensure!(
            !settings.keep_retained || frames == 1,
            "keep-retained requires one frame"
        );
        ensure!(
            !verify_retained
                || (expected_wire_hash.is_some()
                    && !stream
                    && !replay
                    && frames == 1
                    && duo_bin == 1
                    && settings.reopen_after_bytes == 0
                    && settings.interrupt_read_after_bytes == 0
                    && settings.timeout_read_after_bytes == 0),
            "retained verification requires expected interior hash, raw dimensions, one frame, metadata-only output, and no capture fault flags"
        );
        ensure!(
            settings.transfer_timeout_seconds.is_finite()
                && settings.transfer_timeout_seconds > 0.0
                && settings.transfer_timeout_seconds <= 3600.0,
            "invalid transfer deadline"
        );
        settings::read_chunk_bytes(settings.read_chunk_kib)?;
    }
    ensure!(
        args.is_empty()
            || probe_product.is_some()
            || args == ["--probe"]
            || args == ["--probe-all"]
            || args == ["--probe", "--cancel-read"]
            || capture,
        "Usage: regain-device zwo camera-direct [--probe [--cancel-read] | --capture | --capture-585 | --capture-662 | --capture-duo | --capture-2600-p25 | --capture-6200 | --capture-guide] [--width N --height N --x N --y N --microseconds N --gain N --offset N --frames N --read-retries N --transfer-timeout-seconds N --read-chunk-kib N --stream --replay --replay-prefix-bytes N --interrupt-read-after-bytes N]; read chunk size is a power of two from 1 to 1024 KiB (default 1024); ASI585MM Pro/ASI662MC/ASI676MC accept --video --max-fps N (0.01..120, exposures up to 30 s, no replay); ASI2600/6200 also accept --timeout-read-after-bytes N; ASI2600 also accepts --reopen-after-bytes N --reopen-delay-ms N; P25 research: --keep-retained, then --verify-retained-2600-p25 --expected-wire-sha256 HASH with raw width/height; ASI585/2600/6200 and guide also accept --bin N; disconnect other camera apps first"
    );
    // Last resort for a kernel request that refuses to finish cancellation. The
    // worker must exit rather than free a buffer still owned by the USB driver.
    let deadline_seconds = if capture {
        (u64::from(settings.microseconds) / 1_000_000
            + 45
            + (settings.transfer_timeout_seconds.ceil() as u64 + 15)
                * u64::from(settings.read_retries + 1)
                * if replay { 2 } else { 1 }
            + u64::from(settings.reopen_delay_ms) / 1000
            + if video {
                (1.0 / settings.video_max_fps).ceil() as u64
            } else {
                0
            })
            * u64::from(frames)
            + 15
    } else {
        30
    };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(deadline_seconds));
        eprintln!("Direct-driver worker deadline expired");
        std::process::exit(124);
    });
    let mut paths = transport::enumerate()?;
    if let Some(pid) = probe_product {
        paths.retain(|path| path.matches(0x03c3, pid));
    }
    if args == ["--probe-all"] {
        let mut devices = Vec::new();
        for path in &paths {
            devices.push(transport::Camera::open(path)?.probe()?);
        }
        println!(
            "{}",
            serde_json::json!({"sdkLoaded":false,"devices":devices})
        );
        return Ok(());
    }
    if args.is_empty() {
        println!(
            "{}",
            serde_json::json!({"sdkLoaded":false,"interfaces":paths.len()})
        );
        return Ok(());
    }
    if capture {
        let pid = if asi585 {
            0x585e
        } else if asi662 {
            0x662b
        } else if asi6200 {
            0x620b
        } else if p25 {
            0x260e
        } else if duo {
            0x2601
        } else if guide {
            0x2209
        } else {
            0x676d
        };
        paths.retain(|path| path.matches(0x03c3, pid));
    }
    ensure!(
        paths.len() == 1,
        "operation requires exactly one matching camera interface (--capture selects ASI676MC; --capture-duo selects non-P25 ASI2600 main; --capture-2600-p25 selects ASI2600 P25; --capture-guide selects ASI220MM Mini)"
    );
    let camera = transport::Camera::open(&paths[0])?;
    camera.transfer_timeout(settings.transfer_timeout_seconds)?;
    camera.read_chunk_size(settings.read_chunk_kib)?;
    let mut result = camera.probe()?;
    if verify_retained {
        result["retainedVerification"] = asi2600::verify_retained(
            &camera,
            &result,
            &settings,
            expected_wire_hash.as_deref().expect("validated hash"),
        )?;
        println!("{result}");
        return Ok(());
    }
    if capture {
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        let mut video_session = if video {
            Some(bayer_video::Video::start(
                &camera,
                &result,
                if asi585 {
                    asi585::raw_settings(&settings, duo_bin)?
                } else {
                    settings.clone()
                },
                video_profile,
            )?)
        } else {
            None
        };
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let mut pacer = bayer_video::Pacer::default();
        let capture_result = (|| -> Result<()> {
            for frame in 0..frames {
                let (metadata, data) = if let Some(session) = &mut video_session {
                    pacer.wait(settings.video_max_fps, &cancelled)?;
                    let frame = session.next(&camera, &result, &cancelled)?;
                    pacer.completed();
                    if asi585 {
                        asi585::finish(frame, &settings, duo_bin)?
                    } else {
                        frame
                    }
                } else if asi585 {
                    asi585::capture(&camera, &result, &settings, duo_bin, replay)?
                } else if asi662 {
                    asi662::capture(&camera, &result, &settings, replay)?
                } else if asi6200 {
                    asi6200::capture(&camera, &result, &settings, duo_gain, duo_bin, replay)?
                } else if duo {
                    asi2600::capture(&camera, &result, &settings, duo_gain, duo_bin, replay)?
                } else if guide {
                    asi220::capture(&camera, &result, &settings, duo_bin)?
                } else {
                    asi676::capture(&camera, &result, &settings, replay)?
                };
                transport::require_sdk_absent()?;
                result["capture"] = metadata;
                result["capture"]["readChunkKiB"] = serde_json::json!(settings.read_chunk_kib);
                result["frame"] = serde_json::json!(frame);
                if stream {
                    let json = serde_json::to_vec(&result)?;
                    output.write_all(&(json.len() as u32).to_le_bytes())?;
                    output.write_all(&json)?;
                    output.write_all(&data)?;
                } else {
                    writeln!(output, "{}", serde_json::to_string(&result)?)?;
                }
                output.flush()?;
                ensure!(
                    result["capture"].get("cleanupError").is_none(),
                    "frame delivered, but camera must reconnect after cleanup failure: {}",
                    result["capture"]["cleanupError"]
                );
            }
            Ok(())
        })();
        let cleanup = if let Some(session) = &mut video_session {
            session.stop(&camera)
        } else {
            Ok(())
        };
        capture_result?;
        cleanup?;
        return Ok(());
    }
    if args == ["--probe", "--cancel-read"] {
        ensure!(
            result["endpoints"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["address"] == 0x81 && e["attributes"] == 2),
            "observed bulk-IN endpoint 0x81 is absent"
        );
        result["bulkRead"] = camera.cancel_read()?;
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

#[test]
fn invalid_video_cli_options_fail_before_camera_access() {
    for (args, expected) in [
        (
            vec!["--capture-662", "--max-fps", "0.5"],
            "requires --video",
        ),
        (
            vec!["--capture-duo", "--video"],
            "only available for ASI585MM Pro/ASI662MC/ASI676MC",
        ),
        (
            vec!["--capture-662", "--video", "--max-fps", "0"],
            "video maxFps",
        ),
        (
            vec!["--capture-662", "--video", "--read-retries", "1"],
            "retained read retries",
        ),
        (
            vec!["--capture-662", "--video", "--microseconds", "30000001"],
            "30-second",
        ),
    ] {
        let error = run(args.into_iter().map(str::to_owned).collect()).unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}

#[test]
fn descriptor_probe_is_explicit_and_limited_to_supported_products() {
    assert_eq!(
        probe_product(&["--probe-pid".into(), "662b".into()]).unwrap(),
        Some(0x662b)
    );
    assert_eq!(
        probe_product(&["--probe-pid".into(), "0x260e".into()]).unwrap(),
        Some(0x260e)
    );
    assert!(probe_product(&["--probe-pid".into()]).is_err());
    assert!(probe_product(&["--probe-pid".into(), "ffff".into()]).is_err());
    assert!(probe_product(&["--probe-pid".into(), "662b".into(), "--cancel-read".into()]).is_err());
    assert_eq!(probe_product(&["--probe".into()]).unwrap(), None);
}
