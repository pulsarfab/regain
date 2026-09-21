use crate::ofp2::{
    Panel, Transport,
    serial::{self, Port, Serial},
    simulation::{self, Simulation},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::Duration;

fn open(port: &Port, simulate: bool) -> Result<Panel<Box<dyn Transport>>> {
    let transport: Box<dyn Transport> = if simulate {
        Box::new(Simulation::default())
    } else {
        Box::new(Serial::open(&port.port)?)
    };
    Panel::new(transport)
}
fn identity(panel: &Panel<Box<dyn Transport>>, port: &Port, simulate: bool) -> Value {
    let mut value = serde_json::to_value(panel.identity()).unwrap();
    value["serial"] = json!(port.serial);
    value["port"] = json!(port.port);
    value["simulation"] = json!(simulate);
    value
}
fn request(
    panel: &mut Panel<Box<dyn Transport>>,
    port: &Port,
    simulate: bool,
    v: Value,
) -> Result<Value> {
    match v["command"].as_str().unwrap_or("") {
        "identity" => return Ok(identity(panel, port, simulate)),
        "status" => return Ok(serde_json::to_value(panel.status()?)?),
        "open" => panel.move_cover(true)?,
        "close" => panel.move_cover(false)?,
        "halt" => panel.halt()?,
        "off" => panel.light_off()?,
        "on" => {
            let brightness = v["brightness"]
                .as_u64()
                .filter(|v| *v <= 4096)
                .context("Brightness must be an integer in 0..=4096")?;
            panel.light_on(brightness as u16)?;
        }
        _ => bail!("Unknown OFP2 command"),
    }
    Ok(Value::Null)
}
pub fn run(args: Vec<String>) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("help");
    if !["list-details", "status", "serve"].contains(&action) {
        bail!(
            "Usage: regain-device deepskydad ofp2 list-details|status|serve [--serial USB_SERIAL] [--simulate]"
        );
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
                Ok(panel) => found.push(identity(&panel, &port, simulate)),
                Err(e) => eprintln!("{}: {e:#}", port.port),
            }
        }
        println!("{}", json!(found));
        return Ok(());
    }
    let selected = selected.context(
        "Choose the USB serial with --serial (list-details lists verified OFP2 devices)",
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
    let mut panel = open(port, simulate)?;
    if action == "status" {
        println!("{}", serde_json::to_string(&panel.status()?)?);
        return Ok(());
    }
    regain_worker::serve(
        &mut panel,
        Duration::from_millis(250),
        |state, v| request(state, port, simulate, v),
        |state| {
            if state.has_pending_motion() && state.fault().is_none() {
                let _ = state.status();
            }
        },
    );
    if panel.has_pending_motion() && panel.fault().is_none() {
        panel.halt()?;
    }
    Ok(())
}
