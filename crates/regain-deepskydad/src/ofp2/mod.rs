//! Deep Sky Dad OFP2 native USB CDC serial protocol.
//!
//! `Panel` owns a transport exclusively. Operations are serialized, acknowledged
//! once and never replayed after an ambiguous failure. Opening only reads identity
//! and status; it does not change heater configuration, endpoints or illumination.
pub mod serial;
pub mod simulation;

use anyhow::{Result, bail, ensure};
use serde::Serialize;
use std::time::{Duration, Instant};

pub const MAX_BRIGHTNESS: u16 = 4096;
pub const MOTION_TIMEOUT: Duration = Duration::from_secs(120);

/// One framed request and response, without retries. Returns the decoded payload.
pub trait Transport {
    fn exchange(&mut self, command: &str) -> Result<String>;
}

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn exchange(&mut self, command: &str) -> Result<String> {
        (**self).exchange(command)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub model: String,
    pub firmware: String,
    pub product: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverState {
    Closed,
    Open,
    Moving,
    Unknown,
}
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub cover: CoverState,
    pub position_degrees: u16,
    pub light_on: bool,
    /// ASCOM logical on state, including a successful CalibratorOn(0).
    pub calibrator_on: bool,
    pub brightness: u16,
    pub max_brightness: u16,
}
pub struct Panel<T: Transport> {
    transport: T,
    identity: Identity,
    fault: Option<String>,
    motion: Option<(CoverState, Instant)>,
    zero_on: bool,
}
impl<T: Transport> Panel<T> {
    pub fn new(mut transport: T) -> Result<Self> {
        let firmware = transport.exchange("GFRM")?;
        ensure!(
            firmware.starts_with("Board=DeepSkyDad.FP2,"),
            "Not an FP2 board: {firmware}"
        );
        let product = transport.exchange("GPRD")?;
        ensure!(
            product == "3",
            "Only the OFP2 product (3) is supported, got {product}"
        );
        let mut panel = Self {
            transport,
            identity: Identity {
                model: "Deep Sky Dad OFP2".into(),
                firmware,
                product: 3,
            },
            fault: None,
            motion: None,
            zero_on: false,
        };
        panel.status()?;
        Ok(panel)
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }
    pub fn has_pending_motion(&self) -> bool {
        self.motion.is_some()
    }
    fn command(&mut self, command: &str) -> Result<String> {
        if let Some(fault) = &self.fault {
            bail!("Session faulted; reconnect after checking the panel: {fault}");
        }
        let result = self.transport.exchange(command);
        if let Err(e) = &result {
            self.fault = Some(format!("{command}: {e:#}; command was not retried"));
        }
        result
    }
    fn invalid(&mut self, message: String) -> anyhow::Error {
        self.fault = Some(message.clone());
        anyhow::anyhow!(message)
    }
    fn number(&mut self, command: &str, maximum: u16) -> Result<u16> {
        let value = self.command(command)?;
        value
            .parse::<u16>()
            .ok()
            .filter(|v| *v <= maximum)
            .ok_or_else(|| self.invalid(format!("Invalid {command} reply: {value}")))
    }
    fn ack(&mut self, command: &str) -> Result<()> {
        let reply = self.command(command)?;
        if reply != "OK" {
            return Err(self.invalid(format!("Unexpected {command} acknowledgment: {reply}")));
        }
        Ok(())
    }
    pub fn status(&mut self) -> Result<Status> {
        let report = self.number("GOPS", 3)?;
        let position_degrees = self.number("GPOS", 270)?;
        // Firmware 1.0.14.2 reports GOPS=1 after STOP even at GPOS=232.
        // Only claim a completed opening at the requested logical endpoint.
        let cover = match (report, position_degrees) {
            (0, 270) => CoverState::Closed,
            (1, 0) => CoverState::Open,
            (2, _) => CoverState::Moving,
            _ => CoverState::Unknown,
        };
        if let Some((target, since)) = self.motion {
            if cover == target {
                self.motion = None;
            } else if cover != CoverState::Moving || since.elapsed() >= MOTION_TIMEOUT {
                if cover == CoverState::Moving {
                    // The link is still healthy: issue a single stop before
                    // latching the movement timeout, never repeat SMOV.
                    self.ack("STOP")?;
                }
                return Err(self.invalid(format!("Cover failed to reach {target:?}: {cover:?}")));
            }
        }
        let light_on = self.number("GLON", 1)? == 1;
        let brightness = self.number("GLBR", MAX_BRIGHTNESS)?;
        Ok(Status {
            cover,
            position_degrees,
            light_on,
            calibrator_on: light_on || (self.zero_on && brightness == 0),
            brightness,
            max_brightness: MAX_BRIGHTNESS,
        })
    }
    /// Begin a movement to the panel's existing endpoint configuration.
    pub fn move_cover(&mut self, open: bool) -> Result<()> {
        let target = if open {
            CoverState::Open
        } else {
            CoverState::Closed
        };
        let status = self.status()?;
        ensure!(
            status.cover != CoverState::Moving,
            "Cover is already moving"
        );
        if status.cover == target {
            return Ok(());
        }
        self.ack(if open { "STRG0" } else { "STRG270" })?;
        self.ack("SMOV")?;
        self.motion = Some((target, Instant::now()));
        Ok(())
    }
    pub fn halt(&mut self) -> Result<()> {
        self.ack("STOP")?;
        self.motion = None;
        Ok(())
    }
    /// Zero is valid and keeps the calibrator logically on, as ASCOM specifies.
    pub fn light_on(&mut self, brightness: u16) -> Result<()> {
        ensure!(
            brightness <= MAX_BRIGHTNESS,
            "Brightness must be 0..={MAX_BRIGHTNESS}"
        );
        self.ack(&format!("SLBR{brightness}"))?;
        self.ack("SLON1")?;
        // GLON reports physical illumination: firmware returns 0 at brightness
        // zero even after SLON1. Preserve the successful logical On(0) request.
        self.zero_on = brightness == 0;
        Ok(())
    }
    pub fn light_off(&mut self) -> Result<()> {
        self.ack("SLBR0")?;
        self.ack("SLON0")?;
        self.zero_on = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ofp2::simulation::Simulation;

    #[test]
    fn stalled_motion_stops_once_and_latches_a_fault() {
        struct Recording {
            inner: Simulation,
            writes: Vec<String>,
        }
        impl Transport for Recording {
            fn exchange(&mut self, command: &str) -> Result<String> {
                self.writes.push(command.into());
                self.inner.exchange(command)
            }
        }
        let mut p = Panel::new(Recording {
            inner: Simulation::default(),
            writes: vec![],
        })
        .unwrap();
        p.move_cover(false).unwrap();
        p.motion = Some((CoverState::Closed, Instant::now() - MOTION_TIMEOUT));
        assert!(p.status().is_err());
        assert!(p.status().is_err());
        assert!(p.move_cover(false).is_err());
        assert_eq!(
            p.transport.writes.iter().filter(|v| *v == "STOP").count(),
            1
        );
        assert_eq!(
            p.transport.writes.iter().filter(|v| *v == "SMOV").count(),
            1
        );
    }
}

/// Command-line worker used by regain-device.
pub mod worker;
