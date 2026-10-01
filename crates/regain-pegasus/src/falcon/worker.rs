use super::{
    Rotator,
    simulation::{self, Simulation},
};
use crate::{
    Transport,
    serial::{self, Port, Serial},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::Duration;
fn open(p: &Port, simulate: bool) -> Result<Rotator<Box<dyn Transport>>> {
    let io: Box<dyn Transport> = if simulate {
        Box::new(Simulation::default())
    } else {
        Box::new(Serial::open_with_rts(&p.port, Some(true))?)
    };
    Rotator::new(io)
}
fn identity(d: &Rotator<Box<dyn Transport>>, p: &Port, simulate: bool) -> Value {
    json!({"model":"Pegasus Astro Falcon V2","serial":p.serial,"port":p.port,
        "firmware":d.firmware,"deviceId":d.device_id,"simulation":simulate})
}
pub fn run(args: Vec<String>) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("help");
    ensure!(
        ["list-details", "status", "serve"].contains(&action),
        "Usage: regain-device pegasus falcon list-details|status|serve [--serial USB_SERIAL] [--simulate]"
    );
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
            other => bail!("Unknown argument {other}"),
        }
        i += 1;
    }
    let ports = if simulate {
        vec![Port {
            port: "SIMULATION".into(),
            serial: simulation::SERIAL.into(),
        }]
    } else {
        serial::candidates(0x9002)?
    };
    if action == "list-details" {
        let mut found = vec![];
        for p in ports {
            match open(&p, simulate) {
                Ok(mut d) => {
                    found.push(json!({"identity":identity(&d,&p,simulate),"status":d.status()?}))
                }
                Err(e) => eprintln!("{}: {e:#}", p.port),
            }
        }
        println!("{}", json!(found));
        return Ok(());
    }
    let selected = selected.context("Choose a USB serial from list-details")?;
    let matching: Vec<_> = ports
        .into_iter()
        .filter(|p| p.serial.eq_ignore_ascii_case(&selected))
        .collect();
    ensure!(
        matching.len() == 1,
        "Selected Falcon USB serial is missing or ambiguous"
    );
    let p = &matching[0];
    let mut d = open(p, simulate)?;
    if action == "status" {
        println!("{}", serde_json::to_string(&d.status()?)?);
        return Ok(());
    }
    regain_worker::serve(
        &mut d,
        Duration::from_millis(250),
        |d, v| {
            if v["command"] == "identity" {
                Ok(identity(d, p, simulate))
            } else {
                d.request(&v)
            }
        },
        |d| {
            if d.has_pending_motion() && d.fault().is_none() {
                let _ = d.status();
            }
        },
    );
    if d.has_pending_motion() && d.fault().is_none() {
        d.halt()?;
    }
    Ok(())
}
