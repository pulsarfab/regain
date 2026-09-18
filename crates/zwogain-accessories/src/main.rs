use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::mpsc,
    time::{Duration, Instant},
};
use zwogain_accessories::{Accessory, Calibration, Kind, Status, Transport, simulation::Sim};
type Device = Accessory<Box<dyn Transport>>;

fn observe_calibration(
    calibration: &mut Option<Calibration>,
    status: &Status,
    fault: &mut Option<String>,
) {
    if let Some(active) = calibration {
        match active.observe(status, Instant::now()) {
            Ok(true) => *calibration = None,
            Ok(false) => (),
            Err(error) => {
                *fault = Some(format!("{error:#}"));
                *calibration = None;
            }
        }
    }
}

fn discover(kind: Kind, simulate: bool) -> Result<Vec<Device>> {
    if simulate {
        return Ok(vec![Accessory::open(
            Box::new(Sim::new(kind)) as Box<dyn Transport>,
            kind,
        )?]);
    }
    let mut devices = Vec::new();
    for info in zwogain_hid::enumerate(0x03c3, kind.product_id())? {
        match zwogain_hid::Device::open(&info, 0x03c3, kind.product_id())
            .and_then(|d| Accessory::open(Box::new(d) as Box<dyn Transport>, kind))
        {
            Ok(device) => devices.push(device),
            Err(error) => eprintln!("{}: {error:#}", info.path),
        }
    }
    Ok(devices)
}
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let kind = match args.first().map(String::as_str) {
        Some("efw") => Kind::Efw,
        Some("eaf") => Kind::Eaf,
        _ => bail!(
            "usage: zwogain-accessories efw|eaf list-details|status|serve [--serial SERIAL] [--simulate]"
        ),
    };
    let command = args.get(1).map(String::as_str).unwrap_or("status");
    let serial = args
        .iter()
        .position(|v| v == "--serial")
        .map(|i| args.get(i + 1).context("missing serial"))
        .transpose()?;
    let mut devices = discover(kind, args.iter().any(|v| v == "--simulate"))?;
    if command == "list-details" {
        let mut result = Vec::new();
        for d in &mut devices {
            result.push(json!({"identity":d.identity,"status":d.status()?}));
        }
        println!("{}", json!(result));
        return Ok(());
    }
    if let Some(serial) = serial {
        devices.retain(|d| d.identity.serial.eq_ignore_ascii_case(serial));
    }
    ensure!(
        devices.len() == 1,
        "select exactly one available {} by serial; found {}",
        kind.name(),
        devices.len()
    );
    let mut device = devices.remove(0);
    if command == "status" {
        println!(
            "{}",
            json!({"identity":device.identity,"status":device.status()?})
        );
        return Ok(());
    }
    ensure!(command == "serve", "unknown command");
    ensure!(
        kind != Kind::Efw || !device.status()?.moving,
        "Wait for the EFW to stop before connecting; slot detection may be in progress"
    );
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut target: Option<(i32, Instant)> = None;
    let mut fault: Option<String> = None;
    let mut calibration: Option<Calibration> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                let response = (|| -> Result<Value> {
                    let request: Value = serde_json::from_str(&line?)?;
                    let command = request["command"].as_str().context("missing command")?;
                    match command {
                        "identity" => Ok(json!(device.identity)),
                        "status" => {
                            let status = device.status()?;
                            observe_calibration(&mut calibration, &status, &mut fault);
                            let mut v = json!(status);
                            v["fault"] = json!(fault);
                            v["target"] = json!(target.map(|v| v.0));
                            v["calibrating"] = json!(calibration.is_some());
                            if let Some(active) = &calibration {
                                v["detected_slots"] = v["slots"].clone();
                                v["slots"] = json!(active.slots);
                                v["moving"] = json!(true);
                                v["position"] = json!(-1);
                            }
                            if target.is_some() {
                                v["moving"] = json!(true);
                            }
                            Ok(v)
                        }
                        "move" => {
                            ensure!(
                                fault.is_none(),
                                "motion fault: reconnect after inspecting hardware"
                            );
                            ensure!(
                                target.is_none() && calibration.is_none(),
                                "motion or calibration already pending"
                            );
                            let position = i32::try_from(
                                request["position"].as_i64().context("invalid position")?,
                            )?;
                            if let Err(error) = device.move_to(
                                position,
                                request["unidirectional"].as_bool().unwrap_or(false),
                            ) {
                                fault = Some(format!("{error:#}"));
                                if kind == Kind::Eaf {
                                    let _ = device.halt();
                                }
                                return Err(error);
                            }
                            target = Some((
                                position,
                                Instant::now()
                                    + Duration::from_secs(if kind == Kind::Efw { 60 } else { 600 }),
                            ));
                            Ok(json!(null))
                        }
                        "halt" => {
                            ensure!(kind == Kind::Eaf, "EFW has no verified halt command");
                            target = None;
                            device.halt()?;
                            Ok(json!(null))
                        }
                        "settings" => {
                            ensure!(
                                target.is_none() && calibration.is_none(),
                                "motion or calibration pending"
                            );
                            fn boolean(v: &Value, key: &str) -> Result<Option<bool>> {
                                v.get(key)
                                    .map(|v| v.as_bool().context("invalid boolean"))
                                    .transpose()
                            }
                            let backlash = request
                                .get("backlash")
                                .map(|v| -> Result<u8> {
                                    Ok(u8::try_from(v.as_u64().context("invalid backlash")?)?)
                                })
                                .transpose()?;
                            let max_step = request
                                .get("max_step")
                                .map(|v| -> Result<u32> {
                                    Ok(u32::try_from(v.as_u64().context("invalid limit")?)?)
                                })
                                .transpose()?;
                            device.settings(
                                boolean(&request, "beep")?,
                                boolean(&request, "reverse")?,
                                backlash,
                                max_step,
                            )?;
                            Ok(json!(device.status()?))
                        }
                        "calibrate" => {
                            ensure!(kind == Kind::Efw, "calibration applies to EFW only");
                            ensure!(
                                fault.is_none(),
                                "motion fault: reconnect after inspecting hardware"
                            );
                            ensure!(
                                target.is_none() && calibration.is_none(),
                                "motion or calibration already pending"
                            );
                            match device.calibrate() {
                                Ok(active) => calibration = Some(active),
                                Err(error) => {
                                    fault = Some(format!("{error:#}"));
                                    return Err(error);
                                }
                            }
                            Ok(json!(null))
                        }
                        _ => bail!("unknown command {command}"),
                    }
                })();
                let response = match response {
                    Ok(v) => json!({"ok":true,"result":v}),
                    Err(e) => json!({"ok":false,"error":format!("{e:#}")}),
                };
                println!("{response}");
                io::stdout().flush()?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if calibration.is_some() {
            match device.status() {
                Ok(status) => observe_calibration(&mut calibration, &status, &mut fault),
                Err(error) => {
                    fault = Some(format!("calibration status failed: {error:#}"));
                    calibration = None;
                }
            }
        }
        if let Some((position, deadline)) = target {
            match device.status() {
                Ok(s) if !s.moving && s.error == 0 && s.position == position => target = None,
                Ok(s) if s.error == 0 && Instant::now() < deadline => (),
                status => {
                    fault = Some(format!("motion failed or timed out: {status:?}"));
                    target = None;
                    if kind == Kind::Eaf {
                        let _ = device.halt();
                    }
                }
            }
        }
    }
    if kind == Kind::Eaf {
        device.halt()?;
    }
    Ok(())
}
