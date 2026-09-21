use anyhow::{Context, Result, bail, ensure};
use regain_eta::{
    Tilter, Transport,
    serial::{self, Port, Serial},
    simulation::{self, Simulation},
};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::mpsc,
    time::Duration,
};

fn open(port: &Port, simulate: bool) -> Result<Tilter<Box<dyn Transport>>> {
    let transport: Box<dyn Transport> = if simulate {
        Box::new(Simulation::default())
    } else {
        Box::new(Serial::open(&port.port)?)
    };
    Tilter::new(transport)
}
fn identity(tilter: &Tilter<Box<dyn Transport>>, port: &Port, simulate: bool) -> Value {
    json!({"model":"Wanderer Astro ETA M54", "firmware":tilter.firmware(), "serial":port.serial, "port":port.port, "simulation":simulate})
}
fn request(
    tilter: &mut Tilter<Box<dyn Transport>>,
    port: &Port,
    simulate: bool,
    v: Value,
) -> Result<Value> {
    match v["command"].as_str().unwrap_or("") {
        "identity" => return Ok(identity(tilter, port, simulate)),
        "status" => return Ok(serde_json::to_value(tilter.status()?)?),
        "move" => tilter.move_to(i32::try_from(
            v["position"]
                .as_i64()
                .context("Expected integer position")?,
        )?)?,
        "halt" => tilter.halt()?,
        "move-point" => tilter.move_point(
            usize::try_from(v["point"].as_u64().context("Expected point 1..3")?)?,
            i32::try_from(
                v["position"]
                    .as_i64()
                    .context("Expected integer micrometres")?,
            )?,
        )?,
        "cancel-queued" => tilter.cancel_queued(),
        _ => bail!("Unknown ETA M54 command"),
    }
    Ok(Value::Null)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = args.first().map(String::as_str).unwrap_or("help");
    if !["list-details", "status", "serve"].contains(&action) {
        bail!("Usage: regain-eta list-details|status|serve [--serial PORT] [--simulate]");
    }
    let mut simulate = false;
    let mut selected = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--simulate" => simulate = true,
            "--serial" => {
                i += 1;
                selected = Some(args.get(i).context("Missing serial")?.clone());
            }
            v => bail!("Unknown argument {v}"),
        }
        i += 1;
    }
    let ports = if simulate {
        vec![Port {
            port: "SIMULATION".into(),
            serial: simulation::SERIAL.into(),
        }]
    } else {
        serial::candidates()?
    };
    if action == "list-details" {
        let mut found = vec![];
        for port in ports {
            match open(&port, simulate) {
                Ok(mut tilter) => found.push(json!({"identity": identity(&tilter, &port, simulate), "status": tilter.status()?})),
                Err(e) => eprintln!("{}: {e:#}", port.port),
            }
        }
        println!("{}", json!(found));
        return Ok(());
    }
    let selected = selected.context(
        "Choose the port path with --serial (list-details lists verified ETA M54 devices)",
    )?;
    let matching: Vec<_> = ports
        .into_iter()
        .filter(|p| p.serial.eq_ignore_ascii_case(&selected))
        .collect();
    ensure!(matching.len() == 1, "Selected port is missing or ambiguous");
    let port = &matching[0];
    let mut tilter = open(port, simulate)?;
    if action == "status" {
        println!("{}", serde_json::to_string(&tilter.status()?)?);
        return Ok(());
    }
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::spawn(move || {
        let mut input = io::stdin().lock();
        loop {
            // Bound input before allocation; compatible with .NET Framework's initial BOM.
            let mut line = vec![];
            loop {
                let buf = match input.fill_buf() {
                    Ok(v) => v,
                    Err(_) => return,
                };
                if buf.is_empty() {
                    return;
                }
                let end = buf.iter().position(|b| *b == b'\n').map(|n| n + 1);
                let n = end.unwrap_or(buf.len());
                if line.len() + n > 4096 {
                    return;
                }
                line.extend_from_slice(&buf[..n]);
                input.consume(n);
                if end.is_some() {
                    break;
                }
            }
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(line) => {
                let result = String::from_utf8(line)
                    .map_err(anyhow::Error::from)
                    .and_then(|line| Ok(serde_json::from_str(line.trim_start_matches('\u{feff}'))?))
                    .and_then(|v| request(&mut tilter, port, simulate, v));
                let reply = match result {
                    Ok(v) => json!({"ok":true,"result":v}),
                    Err(e) => json!({"ok":false,"error":format!("{e:#}")}),
                };
                if writeln!(io::stdout(), "{reply}")
                    .and_then(|_| io::stdout().flush())
                    .is_err()
                {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if tilter.pending() {
                    let _ = tilter.status();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if tilter.pending() {
        tilter.cancel_queued();
    }
    Ok(())
}
