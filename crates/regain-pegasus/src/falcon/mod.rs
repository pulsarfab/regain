//! Falcon Rotator V2 USB serial protocol. Writes are never retried.
use crate::Transport;
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
pub mod simulation;
pub mod worker;

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub mechanical_degrees: f64,
    pub logical_degrees: f64,
    pub logical_offset: f64,
    pub target_degrees: f64,
    pub moving: bool,
    pub reverse: bool,
    pub speed: u32,
    pub microsteps: u8,
    pub error: u8,
    pub motion_error: Option<String>,
}

#[cfg(test)]
mod tests;
pub fn parse_status(reply: &str) -> Result<Status> {
    let p: Vec<_> = reply.split(':').collect();
    ensure!(p.len() == 6 && p[0] == "F2R", "Invalid Falcon V2 status");
    let position: f64 = p[1].parse()?;
    ensure!(
        position.is_finite() && (0.0..=360.0).contains(&position),
        "Invalid mechanical angle"
    );
    ensure!(
        matches!(p[2], "0" | "1") && matches!(p[5], "0" | "1"),
        "Invalid state flags"
    );
    let microsteps = p[4].parse()?;
    ensure!(
        matches!(microsteps, 2 | 4 | 8 | 16 | 32),
        "Invalid microsteps"
    );
    let speed = p[3].parse()?;
    Ok(Status {
        mechanical_degrees: position,
        logical_degrees: position.rem_euclid(360.),
        logical_offset: 0.,
        target_degrees: position.rem_euclid(360.),
        moving: p[2] == "1",
        reverse: p[5] == "1",
        speed,
        microsteps,
        error: 0,
        motion_error: None,
    })
}
fn angle(degrees: f64) -> Result<()> {
    ensure!(
        degrees.is_finite() && (0.0..360.0).contains(&degrees),
        "Angle must be in 0..<360 degrees"
    );
    Ok(())
}
fn distance(a: f64, b: f64) -> f64 {
    ((a - b + 180.).rem_euclid(360.) - 180.).abs()
}
struct Motion {
    target: f64,
    remaining: f64,
    deadline: Instant,
}
pub struct Rotator<T: Transport> {
    transport: T,
    pub device_id: String,
    pub firmware: Vec<u16>,
    offset: f64,
    target: f64,
    motion: Option<Motion>,
    fault: Option<String>,
}
impl<T: Transport> Rotator<T> {
    pub fn new(mut transport: T) -> Result<Self> {
        let device_id = transport.exchange("F#")?;
        let parts: Vec<_> = device_id.split('_').collect();
        ensure!(
            parts.len() == 3
                && parts[0] == "F2R"
                && (6..=8).contains(&parts[1].len())
                && parts[1].bytes().all(|c| c.is_ascii_hexdigit())
                && parts[2].len() == 1
                && parts[2].bytes().all(|c| c.is_ascii_uppercase()),
            "Not a Falcon V2"
        );
        let version = transport.exchange("FV")?;
        let firmware: Vec<u16> = version
            .strip_prefix("FV:")
            .context("Invalid firmware response")?
            .split('.')
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        ensure!(
            (2..=3).contains(&firmware.len()),
            "Invalid firmware version"
        );
        let mut d = Self {
            transport,
            device_id,
            firmware,
            offset: 0.,
            target: 0.,
            motion: None,
            fault: None,
        };
        let s = d.status()?;
        d.target = s.logical_degrees;
        Ok(d)
    }
    fn exchange(&mut self, command: &str) -> Result<String> {
        ensure!(
            self.fault.is_none(),
            "Falcon session fault; reconnect: {:?}",
            self.fault
        );
        let r = self.transport.exchange(command);
        if let Err(e) = &r {
            self.latch_fault(format!("{e:#}; command was not retried"));
        }
        r
    }
    fn latch_fault(&mut self, message: String) {
        self.fault = Some(message);
        if self.motion.take().is_some() {
            // Stop once after an uncertain motion write/read. Its reply may be
            // stale, so it cannot clear the fault or prove that the motor stopped.
            // No further segment or normal exchange may follow on this stream.
            let _ = self.transport.exchange("FH");
            self.fault
                .as_mut()
                .unwrap()
                .push_str("; best-effort halt attempted, stop unverified");
        }
    }
    fn ack_number(&mut self, prefix: &str, value: f64) -> Result<()> {
        let command = format!("{prefix}:{value:.2}");
        let response = self.exchange(&command)?;
        let parsed = response
            .strip_prefix(&format!("{prefix}:"))
            .and_then(|s| s.parse::<f64>().ok());
        if !parsed.is_some_and(|n| n.is_finite() && (n - value).abs() <= 0.011) {
            self.latch_fault(format!(
                "Unexpected acknowledgement for {command}: {response}; not retried"
            ));
            bail!("{}", self.fault.as_ref().unwrap());
        }
        Ok(())
    }
    fn raw_status(&mut self) -> Result<Status> {
        let reply = self.exchange("FA")?;
        let r = parse_status(&reply);
        if let Err(e) = &r {
            self.latch_fault(format!("{e:#}"));
        }
        r
    }
    pub fn status(&mut self) -> Result<Status> {
        let mut s = self.raw_status()?;
        if let Some(m) = &self.motion {
            let failure = if Instant::now() >= m.deadline {
                Some("Motion deadline exceeded")
            } else if !s.moving && distance(s.mechanical_degrees, m.target) > 0.05 {
                Some("Rotator stopped before its target")
            } else {
                None
            };
            if let Some(message) = failure {
                let stop = self.halt();
                self.fault = Some(format!("{message}; halt: {stop:?}"));
                bail!("{}", self.fault.as_ref().unwrap());
            }
            if !s.moving {
                let remaining = m.remaining;
                self.motion = None;
                if remaining.abs() > 0.001 {
                    self.segment(remaining)?;
                    s = self.raw_status()?;
                }
            }
        }
        s.logical_offset = self.offset;
        s.logical_degrees = (s.mechanical_degrees + self.offset).rem_euclid(360.);
        s.target_degrees = self.target;
        s.moving |= self.motion.is_some();
        Ok(s)
    }
    fn idle(&mut self) -> Result<Status> {
        let s = self.status()?;
        ensure!(!s.moving, "Wait for rotation to stop");
        Ok(s)
    }
    fn start(&mut self, target: f64, remaining: f64) -> Result<()> {
        self.motion = Some(Motion {
            target,
            remaining,
            deadline: Instant::now() + Duration::from_secs(600),
        });
        self.ack_number("MD", target)?;
        Ok(())
    }
    // Explicit multi-turn operation only: re-label between <=90-degree segments.
    // Preserve the sky angle while changing the hardware reference.
    fn segment(&mut self, remaining: f64) -> Result<()> {
        let amount = remaining.abs().min(90.);
        let (start, target) = if remaining > 0. {
            (0., amount)
        } else {
            (amount, 0.)
        };
        self.reference(start)?;
        self.start(target, remaining - remaining.signum() * amount)
    }
    pub fn halt(&mut self) -> Result<()> {
        self.motion = None;
        let response = self.exchange("FH")?;
        if response != "FH:1" {
            self.fault = Some("Invalid halt acknowledgement".into());
            bail!("Invalid halt acknowledgement");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let s = self.raw_status()?;
            if !s.moving {
                self.target = (s.mechanical_degrees + self.offset).rem_euclid(360.);
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.fault = Some("Halt did not stop rotation".into());
                bail!("Halt did not stop rotation");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn reference(&mut self, degrees: f64) -> Result<()> {
        angle(degrees)?;
        let before = self.idle()?;
        self.ack_number("SD", degrees)?;
        let after = self.raw_status()?;
        if after.moving || distance(after.mechanical_degrees, degrees) > 0.05 {
            self.fault = Some("Mechanical reference did not settle".into());
            bail!("Mechanical reference did not settle");
        }
        self.offset = before.logical_degrees - after.mechanical_degrees;
        Ok(())
    }
    pub fn has_pending_motion(&self) -> bool {
        self.motion.is_some()
    }
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }
    pub fn request(&mut self, v: &Value) -> Result<Value> {
        let command = v["command"].as_str().context("Command required")?;
        let degrees = || {
            v["degrees"]
                .as_f64()
                .filter(|n| n.is_finite())
                .context("Finite degrees required")
        };
        match command {
            "status" => return Ok(serde_json::to_value(self.status()?)?),
            "settings" => {
                let s = self.status()?;
                return Ok(json!({"reverse":s.reverse,"speed":s.speed,"microsteps":s.microsteps}));
            }
            "stop" => self.halt()?,
            "sync" => {
                let d = degrees()?;
                angle(d)?;
                let s = self.idle()?;
                self.offset = d - s.mechanical_degrees;
                self.target = d;
            }
            "restore-reference" => {
                let offset = v["offset"]
                    .as_f64()
                    .filter(|offset| offset.is_finite() && (0.0..360.0).contains(offset))
                    .context("Invalid saved logical offset")?;
                let state = self.idle()?;
                self.offset = offset;
                self.target = (state.mechanical_degrees + offset).rem_euclid(360.0);
            }
            "reference" => self.reference(degrees()?)?,
            "reset-origin" => self.reference(0.)?,
            "reverse" => {
                let enabled = v["enabled"].as_bool().context("Boolean enabled required")?;
                let before = self.idle()?;
                // Falcon firmware reverses the motor for us. Negating the MD
                // angle as well would undo the requested reversal.
                let command = format!("FN:{}", u8::from(enabled));
                let response = self.exchange(&command)?;
                if response != command {
                    self.fault = Some("Invalid direction acknowledgement".into());
                    bail!("Invalid direction acknowledgement");
                }
                let after = self.raw_status()?;
                if after.reverse != enabled {
                    self.fault = Some("Direction did not change".into());
                    bail!("Direction did not change");
                }
                self.offset = before.logical_degrees - after.mechanical_degrees;
                self.target = before.logical_degrees;
            }
            "move-mechanical" | "move-to" | "move-relative" => {
                let d = degrees()?;
                if command == "move-relative" {
                    ensure!(d.abs() <= 360., "Relative angle must be within one turn");
                } else {
                    angle(d)?;
                }
                let s = self.idle()?;
                let target = match command {
                    "move-mechanical" => d,
                    "move-to" => (d - self.offset).rem_euclid(360.),
                    _ => (s.mechanical_degrees + d).rem_euclid(360.),
                };
                self.start(target, 0.)?;
                self.target = (target + self.offset).rem_euclid(360.);
            }
            "rotate-unwrapped" => {
                let d = degrees()?;
                ensure!(
                    d != 0. && d.abs() <= 450.,
                    "Explicit travel must be nonzero and within -450..450 degrees"
                );
                let s = self.idle()?;
                let target = (s.logical_degrees + d).rem_euclid(360.);
                self.segment(d)?;
                self.target = target;
            }
            _ => bail!("Unsupported Falcon command"),
        }
        Ok(json!({"accepted":true}))
    }
}
