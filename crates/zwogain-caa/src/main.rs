use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    time::Duration,
};
use zwogain_caa::{
    Caa,
    controller::Controller,
    transport::{self, Device},
};

fn emit(value: Value) {
    println!("{value}");
}
fn exercise(caa: &mut Caa<Device>) -> Result<()> {
    let initial = caa.status()?;
    let settings = caa.settings()?;
    ensure!(
        !initial.moving && initial.error == 0,
        "CAA must be idle and fault-free"
    );
    ensure!(
        initial.limit_degrees >= 10,
        "rotation limit too small for exercise"
    );
    emit(
        json!({"event":"initial","status":initial,"settings":settings,"identity":caa.identity()?}),
    );
    let start = initial.mechanical_degrees;
    // All test positions stay in an eight-degree interval on one side of start.
    let direction = if start + 8.0 <= f64::from(initial.limit_degrees) {
        1.0
    } else {
        -1.0
    };
    ensure!(
        start + direction * 8.0 >= 0.0,
        "no room for bounded motion test"
    );
    let result = (|| -> Result<()> {
        let target = start + direction * 2.0;
        caa.move_mechanical(target)?;
        emit(json!({"event":"mechanical","status":caa.wait_for(target,Duration::from_secs(15))?}));
        caa.sync(123.45)?;
        ensure!(
            (caa.status()?.logical_degrees - 123.45).abs() < 0.001,
            "logical sync mismatch"
        );
        let relative = direction * if settings.reverse { -1.0 } else { 1.0 };
        caa.move_relative(relative)?;
        emit(
            json!({"event":"relative","status":caa.wait_for(target+direction,Duration::from_secs(15))?}),
        );
        caa.move_to(123.45)?;
        emit(json!({"event":"logical","status":caa.wait_for(target,Duration::from_secs(15))?}));
        caa.set_reverse(!settings.reverse)?;
        ensure!(
            (caa.status()?.logical_degrees - 123.45).abs() < 0.1,
            "reverse changed logical angle"
        );
        caa.move_relative(-relative * 0.5)?;
        emit(
            json!({"event":"reversed-relative","status":caa.wait_for(target+direction*0.5,Duration::from_secs(15))?}),
        );
        caa.set_reverse(settings.reverse)?;
        caa.set_beep(!settings.beep)?;
        caa.set_beep(settings.beep)?;
        emit(json!({"event":"settings-roundtrip","settings":caa.settings()?}));
        let limit = initial.limit_degrees - 1;
        if f64::from(limit) >= caa.status()?.mechanical_degrees {
            caa.set_limit(limit)?;
            ensure!(
                caa.move_mechanical(f64::from(limit) + 0.5).is_err(),
                "out-of-limit motion accepted"
            );
            caa.set_limit(initial.limit_degrees)?;
            emit(json!({"event":"limit-roundtrip","limit":initial.limit_degrees}));
        }
        let stop_target = start + direction * 8.0;
        caa.move_mechanical(stop_target)?;
        let during = caa.status()?;
        ensure!(during.moving, "stop test did not observe motion");
        caa.stop()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let stopped = loop {
            let status = caa.status()?;
            if !status.moving {
                break status;
            }
            ensure!(std::time::Instant::now() < deadline, "stop did not finish");
            std::thread::sleep(Duration::from_millis(100));
        };
        ensure!(
            (stopped.mechanical_degrees - stop_target).abs() > 0.1,
            "stop happened after target was reached"
        );
        emit(json!({"event":"stopped","during":during,"status":stopped}));
        Ok(())
    })();
    // Always try all restoration steps, reporting every failure. Do not hide
    // the original fault or claim success when cleanup did not complete.
    let mut cleanup_errors = Vec::new();
    macro_rules! restore {
        ($operation:expr) => {
            if let Err(e) = $operation {
                cleanup_errors.push(format!("{e:#}"));
            }
        };
    }
    restore!(caa.stop());
    restore!(caa.set_limit(initial.limit_degrees));
    restore!(caa.set_reverse(settings.reverse));
    restore!(caa.set_beep(settings.beep));
    restore!(caa.move_mechanical(start));
    restore!(caa.wait_for(start, Duration::from_secs(15)));
    restore!(caa.sync(initial.logical_degrees));
    emit(json!({"event":"restoration","errors":cleanup_errors}));
    result?;
    ensure!(cleanup_errors.is_empty(), "CAA restoration failed");
    let final_status = caa.status()?;
    let final_settings = caa.settings()?;
    ensure!(
        final_status.limit_degrees == initial.limit_degrees
            && final_settings.beep == settings.beep
            && final_settings.reverse == settings.reverse,
        "CAA settings were not restored"
    );
    emit(json!({"event":"complete","status":final_status,"settings":final_settings}));
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        bail!(
            "usage: zwogain-caa list|list-details|status|serve|exercise [--path HID-PATH | --serial SERIAL]"
        );
    };
    if command == "--help" {
        println!(
            "zwogain-caa list|list-details|status|serve|exercise [--path HID-PATH | --serial SERIAL]\nNo SDK required. Exercise moves at most eight degrees from the initial position and restores settings.\nServe accepts one JSON command per line; EOF cancels queued moves and stops the motor."
        );
        return Ok(());
    }
    ensure!(
        matches!(
            command,
            "list" | "list-details" | "status" | "serve" | "exercise"
        ),
        "unknown command {command}"
    );
    ensure!(
        args.len() == 1 || (args.len() == 3 && matches!(args[1].as_str(), "--path" | "--serial")),
        "expected optional --path HID-PATH or --serial SERIAL"
    );
    let devices = transport::enumerate()?;
    if command == "list" {
        emit(serde_json::to_value(devices)?);
        return Ok(());
    }
    if command == "list-details" {
        let mut choices = Vec::new();
        for info in &devices {
            let result = (|| -> Result<_> { Caa::connect(Device::open(info)?)?.identity() })();
            choices.push(match result {
                Ok(identity) => json!({"path":info.path,"identity":identity}),
                Err(e) => json!({"path":info.path,"error":format!("{e:#}")}),
            });
        }
        emit(json!(choices));
        return Ok(());
    }
    let mut caa = if args.len() == 3 && args[1] == "--serial" {
        let mut matches = Vec::new();
        for info in &devices {
            if let Ok(mut candidate) = Device::open(info).and_then(Caa::connect)
                && candidate.identity()?.serial.eq_ignore_ascii_case(&args[2])
            {
                matches.push(candidate);
            }
        }
        ensure!(
            matches.len() == 1,
            "selected CAA serial unavailable or ambiguous; close other controllers"
        );
        matches.pop().unwrap()
    } else {
        let selected = if args.len() == 3 {
            devices
                .iter()
                .find(|d| d.path == args[2])
                .context("selected CAA not found")?
        } else {
            ensure!(
                devices.len() == 1,
                "found {} CAA devices; select one using --serial",
                devices.len()
            );
            &devices[0]
        };
        Caa::connect(Device::open(selected)?)?
    };
    match command {
        "status" => emit(
            json!({"identity":caa.identity()?,"settings":caa.settings()?,"status":caa.status()?}),
        ),
        "exercise" => exercise(&mut caa)?,
        "serve" => {
            let mut controller = Controller::new(caa)?;
            let (tx, rx) = std::sync::mpsc::sync_channel(16);
            std::thread::spawn(move || {
                for line in io::stdin().lock().lines() {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            loop {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(line) => {
                        let result = (|| -> Result<Value> {
                            // .NET Framework's Process.StandardInput can emit a
                            // UTF-8 BOM when its StreamWriter is first accessed.
                            let line = line?;
                            controller.request(&serde_json::from_str(
                                line.trim_start_matches('\u{feff}'),
                            )?)
                        })();
                        emit(match result {
                            Ok(value) => json!({"ok":true,"result":value}),
                            Err(error) => json!({"ok":false,"error":format!("{error:#}")}),
                        });
                        io::stdout().flush()?;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => controller.tick(),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        // Never leave a segmented operation running after its owner exits.
                        controller.halt()?;
                        break;
                    }
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
