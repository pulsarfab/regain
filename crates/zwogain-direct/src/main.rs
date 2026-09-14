//! Research executable; never loaded into NINA or included in the plugin ZIP.
#[cfg(windows)]
mod asi676;
#[cfg(windows)]
mod asi676_tables;
mod processing;
mod protocol;
mod settings;
#[cfg(windows)]
mod transport;
use anyhow::{Result, ensure};

#[cfg(windows)]
fn main() -> Result<()> {
    transport::require_sdk_absent()?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--process-frame"] {
        return processing::process_stream();
    }
    let capture = args.first().is_some_and(|a| a == "--capture");
    let mut settings = settings::Settings::default();
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
            let value: u32 = options
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing option value"))?
                .parse()?;
            match option.as_str() {
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
                "--read-retries" => settings.read_retries = value,
                _ => anyhow::bail!("unknown capture option {option}"),
            }
        }
        settings.validate()?;
        ensure!((1..=20).contains(&frames), "frame count must be 1..20");
    }
    ensure!(
        args.is_empty()
            || args == ["--probe"]
            || args == ["--probe-all"]
            || args == ["--probe", "--cancel-read"]
            || capture,
        "Usage: zwogain-direct [--probe [--cancel-read] | --capture [--width N --height N --x N --y N --microseconds N --gain N --offset N --frames N --read-retries N --stream --replay --replay-prefix-bytes N --interrupt-read-after-bytes N]]; disconnect other camera apps first"
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
        paths.retain(|path| {
            String::from_utf16_lossy(path)
                .to_ascii_lowercase()
                .contains("vid_03c3&pid_676d")
        });
    }
    ensure!(
        paths.len() == 1,
        "operation requires exactly one matching camera interface (capture selects ASI676MC only)"
    );
    let camera = transport::Camera::open(&paths[0])?;
    let mut result = camera.probe()?;
    if capture {
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        for frame in 0..frames {
            let (metadata, data) = asi676::capture(&camera, &result, &settings, replay)?;
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

#[cfg(not(windows))]
fn main() -> Result<()> {
    anyhow::bail!("The direct-driver research executable requires Windows");
}
