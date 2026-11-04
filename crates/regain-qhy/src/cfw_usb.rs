//! CFW3 standalone serial transport. A move replies only when it completes;
//! never send NOW while that completion byte is outstanding.
use anyhow::{Context, Result, bail, ensure};
use regain_transport::serial::{self, FlowControl, SerialPort};
use serde_json::{Value, json};
use std::{
    io::Write,
    time::{Duration, Instant},
};

trait Transport {
    fn query(&mut self, command: &[u8], length: usize) -> Result<Vec<u8>>;
    fn move_to(&mut self, position: u8) -> Result<()>;
    fn completion(&mut self) -> Result<Option<u8>>;
}
struct Serial(Box<dyn SerialPort>);
impl Serial {
    fn open(path: &str) -> Result<Self> {
        Ok(Self(
            serial::Settings {
                baud: 9600,
                flow: FlowControl::None,
                timeout: Duration::from_millis(100),
                dtr: false,
                rts: Some(false),
                settle: Duration::ZERO,
                clear_input: false,
            }
            .open(path)?,
        ))
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.0.bytes_to_read()? == 0,
            "Unexpected serial bytes; reconnect after wheel settles"
        );
        let started = Instant::now();
        let result = self.0.write_all(bytes);
        eprintln!(
            "CFW USB OUT {:?}, {:.3} ms, {:?}",
            String::from_utf8_lossy(bytes),
            started.elapsed().as_secs_f64() * 1000.,
            result
        );
        result.context("Write CFW command; not retried")
    }
}
impl Transport for Serial {
    fn query(&mut self, command: &[u8], length: usize) -> Result<Vec<u8>> {
        self.write(command)?;
        let started = Instant::now();
        let deadline = started + Duration::from_secs(3);
        let result = (0..length)
            .map(|_| serial::read_byte(&mut self.0, deadline))
            .collect::<Result<Vec<_>>>();
        eprintln!(
            "CFW USB IN {result:?}, {:.3} ms",
            started.elapsed().as_secs_f64() * 1000.
        );
        result
    }
    fn move_to(&mut self, position: u8) -> Result<()> {
        self.write(&[b"0123456789ABCDEF"[position as usize]])
    }
    fn completion(&mut self) -> Result<Option<u8>> {
        if self.0.bytes_to_read()? == 0 {
            return Ok(None);
        }
        let byte = serial::read_byte(&mut self.0, Instant::now() + Duration::from_secs(1))?;
        eprintln!("CFW USB move completion {byte:#x}");
        Ok(Some(byte))
    }
}
struct Pending {
    target: u8,
    started: Instant,
    acknowledged: bool,
    motion: super::cfw::Motion,
}
struct Wheel<T> {
    transport: T,
    firmware: String,
    slots: u8,
    position: u8,
    pending: Option<Pending>,
    fault: Option<String>,
}
impl<T: Transport> Wheel<T> {
    fn open(mut transport: T) -> Result<Self> {
        let version = transport.query(b"VRS", 8).context(
            "CFW USB identity unavailable; check the selected port, power and USB control mode (red startup flash)",
        )?;
        ensure!(
            version.len() == 8
                && version.starts_with(b"20")
                && version.iter().all(u8::is_ascii_digit),
            "Not a recognized QHY CFW3 firmware response"
        );
        let slots = super::cfw::decode_slots(transport.query(b"MXP", 1)?[0])?;
        let position = super::cfw::decode_position(transport.query(b"NOW", 1)?[0])?;
        ensure!(position < slots, "Wheel position exceeds slot count");
        Ok(Self {
            transport,
            firmware: String::from_utf8(version)?,
            slots,
            position,
            pending: None,
            fault: None,
        })
    }
    fn read_position(&mut self) -> Result<u8> {
        let position = super::cfw::decode_position(self.transport.query(b"NOW", 1)?[0])?;
        ensure!(position < self.slots, "Wheel position exceeds slot count");
        Ok(position)
    }
    fn poll(&mut self, now: Instant) -> Result<()> {
        if let Some(fault) = &self.fault {
            bail!("Wheel state uncertain: {fault}; inspect and reconnect, no move replay");
        }
        let result = (|| -> Result<()> {
            if let Some(mut pending) = self.pending.take() {
                ensure!(
                    now.duration_since(pending.started) < Duration::from_secs(45),
                    "Wheel move timed out"
                );
                if !pending.acknowledged {
                    if let Some(byte) = self.transport.completion()? {
                        ensure!(
                            super::cfw::decode_position(byte)? == pending.target,
                            "Unexpected move completion"
                        );
                        pending.acknowledged = true;
                    } else {
                        self.pending = Some(pending);
                        return Ok(());
                    }
                }
                let position = self.read_position()?;
                if pending.motion.observe(Some(position), now)? {
                    self.position = position;
                } else {
                    self.pending = Some(pending);
                }
            } else {
                self.position = self.read_position()?;
            }
            Ok(())
        })();
        if let Err(e) = &result {
            self.fault = Some(format!("{e:#}"));
        }
        result
    }
    fn move_to(&mut self, position: i64) -> Result<()> {
        ensure!(self.fault.is_none(), "Wheel fault; inspect and reconnect");
        ensure!(self.pending.is_none(), "Wheel is moving");
        ensure!(
            (0..i64::from(self.slots)).contains(&position),
            "Position outside wheel slots"
        );
        self.poll(Instant::now())?;
        let target = position as u8;
        if self.position == target {
            return Ok(());
        }
        let started = Instant::now();
        self.pending = Some(Pending {
            target,
            started,
            acknowledged: false,
            motion: super::cfw::Motion::new(target, started),
        });
        if let Err(e) = self.transport.move_to(target) {
            self.fault = Some(format!("{e:#}"));
            return Err(e);
        }
        Ok(())
    }
    fn status(&mut self) -> Result<Value> {
        self.poll(Instant::now())?;
        Ok(
            json!({"position":if self.pending.is_some() {-1} else {i32::from(self.position)},
            "slots":self.slots,"moving":self.pending.is_some(),"calibrating":false,"fault":self.fault,"error":0}),
        )
    }
}

pub fn run(args: Vec<String>) -> Result<()> {
    let action = args.first().map(String::as_str).unwrap_or("help");
    ensure!(
        ["list-details", "status", "serve"].contains(&action),
        "Usage: regain-device qhy cfw list-details|status|serve --serial PORT"
    );
    let mut selected = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--serial" => {
                i += 1;
                selected = Some(args.get(i).context("Missing port")?.clone());
            }
            _ => bail!("Unknown CFW argument {}", args[i]),
        }
        i += 1;
    }
    // A generic CP210x serial such as 0001 is not a unique wheel identity.
    // Enumerate candidates without sending wheel commands to unrelated devices.
    let ports = serial::candidates(0x10c4, 0xea60, serial::Selection::PortPath)?;
    if action == "list-details" {
        println!("{}",json!(ports.iter().map(|p| json!({"identity":{"serial":p.port,"port":p.port,"model":"QHY CFW3 USB candidate (verify on connect)"}})).collect::<Vec<_>>()));
        return Ok(());
    }
    let selected = selected.context("Select the wheel's serial port explicitly")?;
    ensure!(
        ports.iter().any(|p| p.port.eq_ignore_ascii_case(&selected)),
        "Selected CP210x port is not present"
    );
    let mut wheel = Wheel::open(Serial::open(&selected)?)?;
    let identity = json!({"model":"QHY CFW3 USB","serial":selected,"port":selected,"firmware":wheel.firmware,"slots":wheel.slots});
    if action == "status" {
        println!("{}", json!({"identity":identity,"status":wheel.status()?}));
        return Ok(());
    }
    regain_worker::serve(
        &mut wheel,
        Duration::from_millis(100),
        |wheel, v| match v["command"].as_str().unwrap_or("") {
            "identity" => Ok(identity.clone()),
            "status" => wheel.status(),
            "move" => {
                ensure!(
                    !v["unidirectional"].as_bool().unwrap_or(false),
                    "CFW USB direction control is unsupported"
                );
                wheel.move_to(
                    v["position"]
                        .as_i64()
                        .context("Expected integer position")?,
                )?;
                Ok(Value::Null)
            }
            _ => bail!("Unsupported CFW command"),
        },
        |wheel| {
            if wheel.pending.is_some() && wheel.fault.is_none() {
                let _ = wheel.poll(Instant::now());
            }
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        position: u8,
        completion: Option<u8>,
        writes: usize,
        queries: usize,
    }
    impl Transport for Fake {
        fn query(&mut self, cmd: &[u8], _: usize) -> Result<Vec<u8>> {
            self.queries += 1;
            Ok(match cmd {
                b"VRS" => b"20181114".to_vec(),
                b"MXP" => b"9".to_vec(),
                b"NOW" => vec![b'0' + self.position],
                _ => unreachable!(),
            })
        }
        fn move_to(&mut self, _: u8) -> Result<()> {
            self.writes += 1;
            Ok(())
        }
        fn completion(&mut self) -> Result<Option<u8>> {
            Ok(self.completion.take())
        }
    }
    fn wheel() -> Wheel<Fake> {
        Wheel::open(Fake {
            position: 0,
            completion: None,
            writes: 0,
            queries: 0,
        })
        .unwrap()
    }
    #[test]
    fn completion_is_consumed_before_position_queries_and_move_is_not_replayed() {
        let mut w = wheel();
        w.move_to(8).unwrap();
        let queries = w.transport.queries;
        w.poll(Instant::now()).unwrap();
        assert_eq!(queries, w.transport.queries);
        assert!(w.move_to(1).is_err());
        w.transport.position = 8;
        w.transport.completion = Some(b'8');
        let now = Instant::now();
        w.poll(now).unwrap();
        assert!(w.pending.is_some());
        w.poll(now + Duration::from_millis(300)).unwrap();
        assert!(w.pending.is_none());
        w.move_to(8).unwrap();
        assert_eq!(1, w.transport.writes);
    }
    #[test]
    fn wrong_ack_and_timeout_latch_fault_without_replaying() {
        for wrong_ack in [false, true] {
            let mut w = wheel();
            w.move_to(4).unwrap();
            if wrong_ack {
                w.transport.completion = Some(b'2');
            }
            assert!(
                w.poll(Instant::now() + Duration::from_secs(if wrong_ack { 0 } else { 46 }))
                    .is_err()
            );
            assert!(w.status().is_err());
            assert!(w.move_to(0).is_err());
            assert_eq!(w.transport.writes, 1);
        }
    }
}
