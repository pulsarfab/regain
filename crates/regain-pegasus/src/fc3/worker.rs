use crate::fc3::{
    Focuser, Transport,
    serial::{self, Port, Serial},
    simulation::{self, Simulation},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

struct Controlled {
    focuser: Focuser<Box<dyn Transport>>,
    port: Port,
    simulate: bool,
}
impl regain_core::focuser::Device for Controlled {
    fn status(&mut self) -> Result<Value> {
        Ok(serde_json::to_value(self.focuser.status()?)?)
    }
    fn move_to(&mut self, position: i32) -> Result<()> {
        self.focuser.move_to(position)
    }
    fn halt(&mut self) -> Result<()> {
        self.focuser.halt()
    }
    fn extra(&mut self, value: Value) -> Result<Value> {
        request(&mut self.focuser, &self.port, self.simulate, value)
    }
}

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

pub fn run(args: Vec<String>) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("help");
    if !["list-details", "status", "serve"].contains(&action) {
        bail!(
            "Usage: regain-device pegasus fc3 list-details|status|serve [--serial USB_SERIAL] [--simulate]"
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
    let started = Instant::now();
    let mut controller = regain_core::focuser::Controller::new(Controlled {
        focuser,
        port: matching_port(port),
        simulate,
    });
    regain_worker::serve(
        &mut controller,
        Duration::from_millis(250),
        |state, v| {
            state.poll(started.elapsed());
            state.request(v, started.elapsed())
        },
        |state| {
            state.poll(started.elapsed());
        },
    );
    controller.shutdown()?;
    Ok(())
}

fn matching_port(port: &Port) -> Port {
    Port {
        port: port.port.clone(),
        serial: port.serial.clone(),
    }
}
