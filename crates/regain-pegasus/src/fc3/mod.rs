//! FocusCube3 newline-delimited serial protocol. No Pegasus, ASCOM or network dependency.
use anyhow::{Result, ensure};
use serde::Serialize;
use std::time::{Duration, Instant};
pub mod serial;
pub mod simulation;

pub const MAX_POSITION: i32 = 1_000_000;
pub trait Transport {
    fn exchange(&mut self, command: &str) -> Result<String>;
}
impl<T: Transport + ?Sized> Transport for Box<T> {
    fn exchange(&mut self, command: &str) -> Result<String> {
        (**self).exchange(command)
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub position: i32,
    pub moving: bool,
    pub temperature_c: Option<f64>,
    pub reverse: bool,
    pub backlash: u16,
    pub speed: u16,
    pub max_step: i32,
    pub error: u8,
    pub fault: Option<String>,
}
fn bit(s: &str) -> Result<bool> {
    ensure!(s == "0" || s == "1", "Invalid boolean: {s}");
    Ok(s == "1")
}
pub fn parse_status(reply: &str) -> Result<Status> {
    let p: Vec<_> = reply.split(':').collect();
    ensure!(p.len() == 6 && p[0] == "FC3", "Invalid FC3 status: {reply}");
    let position = p[1].parse::<i32>()?;
    ensure!(position >= 0, "Invalid position");
    let temperature = p[3].parse::<f64>()?;
    ensure!(temperature.is_finite(), "Invalid temperature");
    let backlash = p[5].parse::<u16>()?;
    ensure!(backlash <= 1000, "Invalid backlash");
    Ok(Status {
        position,
        moving: bit(p[2])?,
        temperature_c: if (-55.0..=125.0).contains(&temperature) {
            Some(temperature)
        } else {
            None
        },
        reverse: bit(p[4])?,
        backlash,
        speed: 0,
        max_step: MAX_POSITION,
        error: 0,
        fault: None,
    })
}
pub struct Focuser<T: Transport> {
    transport: T,
    pub firmware: String,
    fault: Option<String>,
    target: Option<(i32, Instant)>,
}
impl<T: Transport> Focuser<T> {
    pub fn new(mut transport: T) -> Result<Self> {
        let identity = transport.exchange("F#")?;
        ensure!(
            identity.starts_with("FC3_") && identity.len() <= 32,
            "Not a FocusCube3: {identity}"
        );
        let version = transport.exchange("FV")?;
        let firmware = version.strip_prefix("FV:").unwrap_or(&version).to_owned();
        ensure!(
            firmware.split('.').count() >= 2
                && firmware.split('.').all(|v| v.parse::<u16>().is_ok()),
            "Invalid firmware version"
        );
        let mut d = Self {
            transport,
            firmware,
            fault: None,
            target: None,
        };
        d.status()?;
        Ok(d)
    }
    fn exchange(&mut self, cmd: &str) -> Result<String> {
        ensure!(
            self.fault.is_none(),
            "Session fault; reconnect after inspecting hardware: {:?}",
            self.fault
        );
        let r = self.transport.exchange(cmd);
        if let Err(e) = &r {
            self.fault = Some(format!("{e:#}"));
        }
        r
    }
    fn ack(&mut self, cmd: &str, expected: &str) -> Result<()> {
        let reply = self.exchange(cmd)?;
        if reply != expected && reply != expected.split_once(':').map_or(expected, |(_, v)| v) {
            self.fault = Some(format!(
                "Unexpected acknowledgement for {cmd}: {reply}; command was not retried"
            ));
        }
        ensure!(
            self.fault.is_none(),
            "{}",
            self.fault.as_deref().unwrap_or("")
        );
        Ok(())
    }
    pub fn status(&mut self) -> Result<Status> {
        let result = (|| -> Result<Status> {
            let mut s = parse_status(&self.exchange("FA")?)?;
            let speed = self.exchange("SP")?;
            s.speed = speed.strip_prefix("SP:").unwrap_or(&speed).parse()?;
            ensure!(s.speed <= 400, "Invalid speed");
            if let Some((target, deadline)) = self.target {
                if Instant::now() >= deadline {
                    // The stream is synchronized, so stop once before latching the fault.
                    let _ = self.halt();
                    anyhow::bail!("Motion deadline exceeded");
                }
                if !s.moving {
                    ensure!(
                        s.position == target,
                        "Focuser stopped before target {target} at {}",
                        s.position
                    );
                    self.target = None;
                }
            }
            Ok(s)
        })();
        if let Err(e) = &result {
            self.fault = Some(format!("{e:#}"));
        }
        result
    }
    pub fn move_to(&mut self, position: i32) -> Result<()> {
        ensure!(
            (0..=MAX_POSITION).contains(&position),
            "Position must be in 0..={MAX_POSITION}"
        );
        ensure!(self.target.is_none(), "Movement already in progress");
        let status = self.status()?;
        ensure!(!status.moving, "Movement already in progress");
        ensure!(
            status.speed >= 2,
            "Set an even motor speed in 2..=400 before moving"
        );
        let cmd = format!("FM:{position}");
        self.ack(&cmd, &cmd)?;
        self.target = Some((position, Instant::now() + Duration::from_secs(600)));
        Ok(())
    }
    pub fn halt(&mut self) -> Result<()> {
        // A fresh transport is required after a framing fault: never consume a stale reply.
        self.ack("FH", "FH:1")?;
        self.target = None;
        // FH acknowledges the request before the motor finishes decelerating.
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = (|| -> Result<()> {
            loop {
                if !parse_status(&self.exchange("FA")?)?.moving {
                    return Ok(());
                }
                ensure!(Instant::now() < deadline, "Focuser did not stop after halt");
                std::thread::sleep(Duration::from_millis(50));
            }
        })();
        if let Err(e) = &result {
            self.fault = Some(format!("{e:#}"));
        }
        result
    }
    pub fn settings(
        &mut self,
        speed: Option<u16>,
        backlash: Option<u16>,
        reverse: Option<bool>,
    ) -> Result<()> {
        ensure!(
            speed.is_none_or(|v| (2..=400).contains(&v) && v % 2 == 0),
            "Speed must be an even integer in 2..=400 (firmware rounds odd values down)"
        );
        ensure!(
            backlash.is_none_or(|v| v <= 1000),
            "Backlash must be in 0..=1000"
        );
        ensure!(
            self.target.is_none() && !self.status()?.moving,
            "Wait for motion to stop before changing settings"
        );
        for (cmd, value) in [
            ("SP", speed),
            ("BL", backlash),
            ("FD", reverse.map(u16::from)),
        ] {
            if let Some(v) = value {
                let c = format!("{cmd}:{v}");
                self.ack(&c, &c)?;
            }
        }
        Ok(())
    }
    pub fn has_pending_motion(&self) -> bool {
        self.target.is_some()
    }
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_status() {
        assert_eq!(parse_status("FC3:42:0:22.5:1:1000").unwrap().position, 42);
        for s in [
            "FC3:0:2:22:0:0",
            "FC3:0:0:NaN:0:0",
            "FC3:-1:0:20:0:0",
            "FC3:0:0:20:0:1001",
            "FC3:1:0:20:0:0:extra",
        ] {
            assert!(parse_status(s).is_err(), "{s}");
        }
    }
    #[test]
    fn validation_and_motion() {
        let mut d = Focuser::new(simulation::Simulation::default()).unwrap();
        assert!(d.move_to(-1).is_err());
        assert!(d.settings(Some(0), None, None).is_err());
        d.move_to(100).unwrap();
        assert!(d.move_to(101).is_err());
        d.halt().unwrap();
        d.settings(Some(100), Some(10), Some(true)).unwrap();
        let s = d.status().unwrap();
        assert_eq!(s.speed, 100);
        assert!(s.reverse);
    }
    #[test]
    fn uncertain_write_latches_without_replay() {
        struct Bad {
            inner: simulation::Simulation,
            writes: usize,
        }
        impl Transport for Bad {
            fn exchange(&mut self, c: &str) -> Result<String> {
                if c.starts_with("FM:") {
                    self.writes += 1;
                    anyhow::bail!("timeout");
                }
                self.inner.exchange(c)
            }
        }
        let mut d = Focuser::new(Bad {
            inner: simulation::Simulation::default(),
            writes: 0,
        })
        .unwrap();
        assert!(d.move_to(1).is_err());
        assert!(d.move_to(2).is_err());
        assert_eq!(d.transport.writes, 1);
    }
    #[test]
    fn halt_waits_for_deceleration_without_repeating_command() {
        struct Deceleration {
            inner: simulation::Simulation,
            polls: usize,
            halts: usize,
        }
        impl Transport for Deceleration {
            fn exchange(&mut self, c: &str) -> Result<String> {
                if c == "FH" {
                    self.halts += 1;
                    self.polls = 2;
                    return Ok("1".into());
                }
                if c == "FA" && self.polls > 0 {
                    self.polls -= 1;
                    return Ok("FC3:1000:1:27.7:0:0".into());
                }
                self.inner.exchange(c)
            }
        }
        let mut d = Focuser::new(Deceleration {
            inner: simulation::Simulation::default(),
            polls: 0,
            halts: 0,
        })
        .unwrap();
        d.halt().unwrap();
        assert_eq!(d.transport.halts, 1);
        assert_eq!(d.transport.polls, 0);
        assert!(d.settings(Some(1), None, None).is_err());
        assert!(d.settings(Some(399), None, None).is_err());
        d.settings(Some(398), None, None).unwrap();
    }
    #[test]
    fn deadline_stops_once_and_latches_fault() {
        let mut d = Focuser::new(simulation::Simulation::default()).unwrap();
        d.move_to(1100).unwrap();
        d.target = Some((1100, Instant::now()));
        assert!(d.status().is_err());
        assert!(d.fault().is_some());
        assert!(
            !parse_status(&d.transport.exchange("FA").unwrap())
                .unwrap()
                .moving
        );
        assert!(d.move_to(1000).is_err());
    }
}

/// Command-line worker used by regain-device.
pub mod worker;
