//! Isolated experimental direct backend; SDK remains the plugin's default.
mod asi220;
mod asi220_tables;
mod asi2600;
mod asi2600_p25_tables;
mod asi2600_tables;
mod asi6200;
mod asi6200_tables;
mod asi676;
mod asi676_tables;
mod completion;
mod diagnostics;
mod environment;
mod processing;
mod protocol;
mod server;
mod settings;
mod transport;
use anyhow::{Result, ensure};

fn main() -> Result<()> {
    transport::require_sdk_absent()?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--serve"] || args == ["--serve", "--simulate"] {
        return server::run(args.len() == 2);
    }
    if args == ["--process-frame"] {
        return processing::process_stream();
    }
    let asi6200 = args.first().is_some_and(|a| a == "--capture-6200");
    let p25 = args.first().is_some_and(|a| a == "--capture-2600-p25");
    let duo = p25 || args.first().is_some_and(|a| a == "--capture-duo");
    let guide = args.first().is_some_and(|a| a == "--capture-guide");
    let capture = asi6200 || duo || guide || args.first().is_some_and(|a| a == "--capture");
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
    let mut frames = 1_u32;
    let mut stream = false;
    let mut replay = false;
    if capture {
        let mut options = args[1..].iter();
        while let Some(option) = options.next() {
            if option == "--replay" {
                replay = true;
                continue;
            }
            if option == "--stream" {
                stream = true;
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
                "--bin" if duo || guide || asi6200 => duo_bin = value,
                "--width" => settings.width = value,
                "--height" => settings.height = value,
                "--x" => settings.x = value,
                "--y" => settings.y = value,
                "--microseconds" => settings.microseconds = value,
                "--gain" => settings.gain = value,
                "--offset" => settings.offset = value,
                "--frames" => frames = value,
                "--replay-prefix-bytes" => {
                    settings.replay_prefix_bytes = value;
                    replay = true;
                }
                "--interrupt-read-after-bytes" => settings.interrupt_read_after_bytes = value,
                "--timeout-read-after-bytes" if duo || asi6200 => {
                    settings.timeout_read_after_bytes = value
                }
                "--read-retries" => settings.read_retries = value,
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
        } else {
            settings.validate()?;
        }
        ensure!((1..=20).contains(&frames), "frame count must be 1..20");
    }
    ensure!(
        args.is_empty()
            || args == ["--probe"]
            || args == ["--probe-all"]
            || args == ["--probe", "--cancel-read"]
            || capture,
        "Usage: zwogain-direct [--probe [--cancel-read] | --capture | --capture-duo | --capture-2600-p25 | --capture-6200 | --capture-guide] [--width N --height N --x N --y N --microseconds N --gain N --offset N --frames N --read-retries N --stream --replay --replay-prefix-bytes N --interrupt-read-after-bytes N]; ASI2600/6200 also accept --timeout-read-after-bytes N; ASI2600/6200 and guide also accept --bin N; disconnect other camera apps first"
    );
    // Last resort for a kernel request that refuses to finish cancellation. The
    // worker must exit rather than free a buffer still owned by the USB driver.
    let deadline_seconds = if capture {
        (u64::from(settings.microseconds) / 1_000_000 + 15) * u64::from(frames) + 15
    } else {
        30
    };
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(deadline_seconds));
        eprintln!("Direct-driver worker deadline expired");
        std::process::exit(124);
    });
    let mut paths = transport::enumerate()?;
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
        let pid = if asi6200 {
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
    let mut result = camera.probe()?;
    if capture {
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        for frame in 0..frames {
            let (metadata, data) = if asi6200 {
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
        return Ok(());
    }
    if args.len() == 2 {
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
