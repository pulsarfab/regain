use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{self, BufRead, Write},
    sync::mpsc,
    time::Duration,
};
use zwogain_fc3::{
    Focuser, Transport,
    serial::{self, Port, Serial},
    simulation::{self, Simulation},
};

fn open(port: &Port, simulate: bool) -> Result<Focuser<Box<dyn Transport>>> {
    let transport: Box<dyn Transport> = if simulate {
        Box::new(Simulation::default())
    } else {
        Box::new(Serial::open(&port.port)?)
    };
    Focuser::new(transport)
}
fn identity(focuser: &Focuser<Box<dyn Transport>>, port: &Port, simulate: bool) -> Value {
    json!({"model":"Pegasus Astro FocusCube3", "firmware":focuser.firmware, "serial":port.serial, "port":port.port, "simulation":simulate})
}
fn request(
    focuser: &mut Focuser<Box<dyn Transport>>,
    port: &Port,
    simulate: bool,
    v: Value,
) -> Result<Value> {
    fn number(v: &Value, key: &str) -> Result<Option<u16>> {
        v.get(key)
            .map(|x| {
                Ok(u16::try_from(
                    x.as_u64().context("Expected unsigned integer")?,
                )?)
            })
            .transpose()
    }
    match v["command"].as_str().unwrap_or("") {
        "identity" => return Ok(identity(focuser, port, simulate)),
        "status" => return Ok(serde_json::to_value(focuser.status()?)?),
        "move" => focuser.move_to(i32::try_from(
            v["position"]
                .as_i64()
                .context("Expected integer position")?,
        )?)?,
        "halt" => focuser.halt()?,
        "settings" => {
            ensure!(
                v.as_object()
                    .context("Expected object")?
                    .keys()
                    .all(|k| ["command", "speed", "backlash", "reverse"].contains(&k.as_str())),
                "Unknown setting"
            );
            let reverse = v
                .get("reverse")
                .map(|x| x.as_bool().context("Expected boolean reverse"))
                .transpose()?;
            focuser.settings(number(&v, "speed")?, number(&v, "backlash")?, reverse)?;
        }
        _ => bail!("Unknown FocusCube3 command"),
    }
    Ok(Value::Null)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = args.first().map(String::as_str).unwrap_or("help");
    if !["list-details", "status", "serve"].contains(&action) {
        bail!("Usage: zwogain-fc3 list-details|status|serve [--serial USB_SERIAL] [--simulate]");
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
                Ok(mut focuser) => found.push(json!({"identity": identity(&focuser, &port, simulate), "status": focuser.status()?})),
                Err(e) => eprintln!("{}: {e:#}", port.port),
            }
        }
        println!("{}", json!(found));
        return Ok(());
    }
    let selected = selected.context(
        "Choose the USB serial with --serial (list-details lists verified FocusCube3 devices)",
    )?;
    let matching: Vec<_> = ports
        .into_iter()
        .filter(|p| p.serial.eq_ignore_ascii_case(&selected))
        .collect();
    ensure!(
        matching.len() == 1,
        "Selected USB serial is missing or ambiguous"
    );
    let port = &matching[0];
    let mut focuser = open(port, simulate)?;
    if action == "status" {
        println!("{}", serde_json::to_string(&focuser.status()?)?);
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
                    .and_then(|v| request(&mut focuser, port, simulate, v));
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
                if focuser.has_pending_motion() && focuser.fault().is_none() {
                    let _ = focuser.status();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if focuser.has_pending_motion() && focuser.fault().is_none() {
        focuser.halt()?;
    }
    Ok(())
}
