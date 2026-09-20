//! Standalone SDK exercises; the pipe protocol remains the default entry point.
use crate::{Exposure, Host};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::PathBuf,
    time::{Duration, Instant},
};

const HELP: &str = "PulsarFab regain SDK camera commands:
  regain-host --list [--sdk /absolute/path/to/library]
  regain-host --inspect [--camera NAME] [--serial HEX]
  regain-host --capture --output NEW_DIRECTORY [--camera NAME] [--serial HEX]
    [--width 256 --height 256 --bin 1 --x 0 --y 0 --microseconds 100000]
    [--frames 1 --read-retries 2] [--gain N --offset N] [--light]
  --inspect and --capture also accept --set CONTROL=VALUE and --hold-seconds N.
  Settings changed with --set, --gain, and --offset are restored before exit.
  Supported --set controls: gain 0, offset 5, USB bandwidth 6, cooler target 16,
  cooler enable 17, dew heater 21, fan 22, LED 23 (when advertised by the SDK).
  --capture writes little-endian RAW16 files and JSON metadata; dark by default.
  --simulate runs these commands without a camera or SDK.
  Without a command, the worker serves the version-1 stdin/stdout protocol.";

#[derive(Debug, PartialEq)]
enum Mode {
    List,
    Inspect,
    Capture,
}

pub struct Options {
    mode: Mode,
    name: Option<String>,
    serial: Option<String>,
    output: Option<PathBuf>,
    exposure: Exposure,
    frames: u32,
    retries: u32,
    hold: u64,
    controls: BTreeMap<i32, i64>,
}

impl Options {
    pub fn parse(args: &[String]) -> Result<Option<Self>> {
        let mut options = Self {
            mode: Mode::List,
            name: None,
            serial: None,
            output: None,
            exposure: Exposure {
                width: 256,
                height: 256,
                bin: 1,
                x: 0,
                y: 0,
                microseconds: 100000,
                dark: true,
            },
            frames: 1,
            retries: 2,
            hold: 0,
            controls: BTreeMap::new(),
        };
        let mut mode = None;
        let mut cli_option = false;
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            let next = |iter: &mut std::slice::Iter<'_, String>| -> Result<String> {
                iter.next()
                    .cloned()
                    .with_context(|| format!("missing value for {arg}"))
            };
            match arg.as_str() {
                "--sdk" => {
                    next(&mut iter)?;
                }
                "--simulate" => {}
                "--list" | "--inspect" | "--capture" => {
                    ensure!(mode.is_none(), "choose one camera command");
                    mode = Some(match arg.as_str() {
                        "--list" => Mode::List,
                        "--inspect" => Mode::Inspect,
                        _ => Mode::Capture,
                    });
                }
                _ => {
                    cli_option = true;
                    match arg.as_str() {
                        "--camera" => options.name = Some(next(&mut iter)?),
                        "--serial" => options.serial = Some(next(&mut iter)?),
                        "--output" => options.output = Some(next(&mut iter)?.into()),
                        "--width" => options.exposure.width = next(&mut iter)?.parse()?,
                        "--height" => options.exposure.height = next(&mut iter)?.parse()?,
                        "--bin" => options.exposure.bin = next(&mut iter)?.parse()?,
                        "--x" => options.exposure.x = next(&mut iter)?.parse()?,
                        "--y" => options.exposure.y = next(&mut iter)?.parse()?,
                        "--microseconds" => {
                            options.exposure.microseconds = next(&mut iter)?.parse()?
                        }
                        "--frames" => options.frames = next(&mut iter)?.parse()?,
                        "--read-retries" => options.retries = next(&mut iter)?.parse()?,
                        "--hold-seconds" => options.hold = next(&mut iter)?.parse()?,
                        "--light" => options.exposure.dark = false,
                        "--gain" | "--offset" => {
                            options.controls.insert(
                                if arg == "--gain" { 0 } else { 5 },
                                next(&mut iter)?.parse()?,
                            );
                        }
                        "--set" => {
                            let value = next(&mut iter)?;
                            let (control, value) =
                                value.split_once('=').context("use --set CONTROL=VALUE")?;
                            options.controls.insert(control.parse()?, value.parse()?);
                        }
                        _ => bail!("unknown option {arg}; use --help"),
                    }
                }
            }
        }
        let Some(mode) = mode else {
            ensure!(!cli_option, "camera options need --inspect or --capture");
            return Ok(None);
        };
        options.mode = mode;
        ensure!(
            options.mode != Mode::List || !cli_option,
            "--list takes only --sdk or --simulate"
        );
        ensure!(
            options.mode == Mode::Capture || options.output.is_none(),
            "--output requires --capture"
        );
        ensure!(
            options.mode != Mode::Capture || options.output.is_some(),
            "--capture requires --output NEW_DIRECTORY"
        );
        options.exposure.size()?;
        ensure!(
            options.exposure.microseconds <= 2_000_000_000,
            "exposure exceeds 2,000 seconds"
        );
        ensure!(
            (1..=20).contains(&options.frames) && options.retries <= 5 && options.hold <= 3600,
            "invalid frame, retry, or hold limit"
        );
        ensure!(
            options
                .controls
                .keys()
                .all(|c| [0, 5, 6, 16, 17, 21, 22, 23].contains(c)),
            "unsupported --set control"
        );
        Ok(Some(options))
    }

    pub fn arm_watchdog(&self) {
        let seconds = self.hold
            + u64::from(self.frames)
                * (self.exposure.microseconds as u64 / 1_000_000
                    + 30
                    + 60 * u64::from(self.retries + 1))
            + 60;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds));
            eprintln!(
                "SDK command deadline expired; terminating. Camera settings may need restoring."
            );
            std::process::exit(124);
        });
    }
}

pub fn help() {
    println!("{HELP}");
}

pub fn run(host: &mut Host, options: Options) -> Result<()> {
    let version = host
        .sdk
        .as_ref()
        .map(|sdk| sdk.version())
        .transpose()?
        .unwrap_or_else(|| "simulator".into());
    let cameras = host.command("list", Value::Null)?.0;
    if options.mode == Mode::List {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"sdkVersion":version,"cameras":cameras}))?
        );
        return Ok(());
    }
    let cameras = cameras.as_array().context("invalid camera list")?;
    let name = match options.name.as_deref() {
        Some(name) => name,
        None => {
            ensure!(
                cameras.len() == 1,
                "use --camera NAME when more than one camera is attached"
            );
            cameras[0]["name"].as_str().context("missing camera name")?
        }
    };
    if let Some(directory) = &options.output {
        fs::create_dir(directory)
            .context("output directory must be new and its parent must exist")?;
    }
    let opened = host
        .command("open", json!({"name":name,"serial":options.serial}))?
        .0;
    let mut saved = Vec::new();
    let result = (|| -> Result<()> {
        // Validate and snapshot every requested setting before changing any.
        let caps = opened["controls"].as_array().context("missing controls")?;
        for (&control, &value) in &options.controls {
            let cap = caps
                .iter()
                .find(|c| c["type"] == control)
                .context("control not advertised")?;
            ensure!(
                cap["writable"] == true
                    && value >= cap["min"].as_i64().context("invalid control minimum")?
                    && value <= cap["max"].as_i64().context("invalid control maximum")?,
                "control {control} value is not supported"
            );
            let state = host
                .command("get-control-state", json!({"control":control}))?
                .0;
            saved.push((control, state));
        }
        for (&control, &value) in &options.controls {
            host.command(
                "set-control-state",
                json!({"control":control,"value":value,"auto":false}),
            )?;
            let actual = host
                .command("get-control-state", json!({"control":control}))?
                .0;
            ensure!(
                actual["value"] == value && actual["auto"] == false,
                "control {control} did not accept requested value"
            );
        }
        std::thread::sleep(Duration::from_secs(options.hold));
        if options.mode == Mode::Inspect {
            let mut info = opened.clone();
            for cap in info["controls"]
                .as_array_mut()
                .context("missing controls")?
            {
                cap["value"] = host.command("get", json!({"control":cap["type"]}))?.0;
            }
            println!("{}", serde_json::to_string_pretty(&info)?);
            return Ok(());
        }
        let e = &options.exposure;
        let exposure = json!({"width":e.width,"height":e.height,"bin":e.bin,"x":e.x,"y":e.y,"microseconds":e.microseconds,"dark":e.dark});
        for index in 1..=options.frames {
            host.command("start", exposure.clone())?;
            let deadline = Instant::now()
                + Duration::from_micros(e.microseconds as u64)
                + Duration::from_secs(30);
            loop {
                match host.command("status", Value::Null)?.0.as_i64() {
                    Some(2) => break,
                    Some(1) => ensure!(Instant::now() < deadline, "exposure readiness timeout"),
                    state => bail!("exposure did not complete: status {state:?}"),
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let mut retries = 0;
            let (mut metadata, pixels) = loop {
                match host.command("download", Value::Null) {
                    Ok(frame) => break frame,
                    Err(error) => {
                        if retries >= options.retries || host.command("status", Value::Null)?.0 != 2
                        {
                            return Err(error);
                        }
                        retries += 1;
                        eprintln!(
                            "Retrying ready-frame download ({retries}/{}): {error:#}",
                            options.retries
                        );
                    }
                }
            };
            ensure!(pixels.len() == e.size()?, "incorrect frame length");
            metadata["exposure"] = exposure.clone();
            metadata["sdkVersion"] = json!(version);
            metadata["camera"] = json!(name);
            metadata["serial"] = opened["serial"].clone();
            metadata["readRetriesUsed"] = json!(retries);
            let path = options
                .output
                .as_ref()
                .unwrap()
                .join(format!("frame-{index:04}.raw"));
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?
                .write_all(&pixels)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path.with_extension("json"))?
                .write_all(&serde_json::to_vec_pretty(&metadata)?)?;
            println!(
                "{}",
                json!({"file":path,"bytes":pixels.len(),"readRetriesUsed":retries})
            );
        }
        Ok(())
    })();
    let mut cleanup = Vec::new();
    if host.exposure.is_some()
        && let Err(error) = host.command("stop", Value::Null)
    {
        cleanup.push(format!("stop: {error:#}"));
    }
    // Restore target before cooler enable, using the same order as application.
    for (control, state) in saved {
        if let Err(error) = host.command(
            "set-control-state",
            json!({"control":control,"value":state["value"],"auto":state["auto"]}),
        ) {
            cleanup.push(format!("restore control {control}: {error:#}"));
        }
    }
    if let Err(error) = host.command("close", Value::Null) {
        cleanup.push(format!("close: {error:#}"));
    }
    for error in &cleanup {
        eprintln!("Cleanup failed: {error}");
    }
    result?;
    ensure!(
        cleanup.is_empty(),
        "camera settings were not fully restored"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Option<Options>> {
        Options::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
    #[test]
    fn invalid_commands_fail_before_loading_sdk_or_opening_camera() {
        for args in [
            &["--capture"][..],
            &["--list", "--inspect"],
            &["--inspect", "--set", "18=1"],
            &["--capture", "--output", "test", "--microseconds", "-1"],
            &["--width", "8"],
            &["--sdk"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
        assert!(
            parse(&["--sdk", "/path/libASICamera2.so"])
                .unwrap()
                .is_none()
        );
        assert_eq!(parse(&["--list"]).unwrap().unwrap().mode, Mode::List);
    }
}
