//! Wanderer ETA M54: three absolute actuator positions, expressed in micrometres.
//! No SDK, Empire service, homing, or undocumented stop command is used.
pub mod serial;
pub mod simulation;

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub const MAX_POSITION: i32 = 1200;
pub const TOLERANCE_UM: f64 = 2.0;
#[derive(Debug, Clone, Serialize)]
pub struct Telemetry {
    pub firmware: String,
    pub points_um: [f64; 3],
    /// Firmware-specific trailing fields, intentionally uninterpreted.
    pub extra: Vec<String>,
}
pub fn parse(frame: &str) -> Result<Telemetry> {
    let fields: Vec<_> = frame.trim_end_matches(['\r', '\n']).split('A').collect();
    ensure!(
        fields.len() >= 6 && fields[0] == "WandererTilterM54" && fields.last() == Some(&""),
        "Invalid ETA M54 telemetry"
    );
    ensure!(
        !fields[1].is_empty() && fields[1].bytes().all(|b| b.is_ascii_digit()),
        "Invalid firmware"
    );
    let mut points_um = [0.0; 3];
    for (point, field) in points_um.iter_mut().zip(&fields[2..5]) {
        *point = field.parse::<f64>().context("Invalid encoder position")? * 1000.0;
        // Encoders can read slightly below zero at the mechanical limit.
        ensure!(
            point.is_finite() && (-50.0..=1250.0).contains(point),
            "Encoder position outside plausible travel"
        );
    }
    Ok(Telemetry {
        firmware: fields[1].into(),
        points_um,
        extra: fields[5..fields.len() - 1]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    })
}
pub fn command(point: usize, target_um: i32) -> Result<String> {
    ensure!((1..=3).contains(&point), "Point must be 1, 2, or 3");
    ensure!(
        (0..=MAX_POSITION).contains(&target_um),
        "Point target must be 0..1200 micrometres"
    );
    Ok(format!("{}{:.3}\n", point, target_um as f64 / 1000.0))
}
pub trait Transport {
    /// Read a fresh complete telemetry frame, never a cached position.
    fn read(&mut self) -> Result<Telemetry>;
    /// Write once. An uncertain write must never be retried automatically.
    fn send(&mut self, point: usize, target_um: i32) -> Result<()>;
}
impl<T: Transport + ?Sized> Transport for Box<T> {
    fn read(&mut self) -> Result<Telemetry> {
        (**self).read()
    }
    fn send(&mut self, point: usize, target_um: i32) -> Result<()> {
        (**self).send(point, target_um)
    }
}
#[derive(Serialize)]
pub struct Status {
    pub position: i32,
    pub points_um: [f64; 3],
    pub moving: bool,
    pub max_step: i32,
    pub tolerance_um: f64,
    pub halt_supported: bool,
    pub firmware: String,
    pub extra: Vec<String>,
    pub error: i32,
    pub fault: Option<String>,
}
struct Motion {
    point: usize,
    target: i32,
    started: Instant,
    settled: u8,
}
pub struct Tilter<T: Transport> {
    transport: T,
    telemetry: Telemetry,
    active: Option<Motion>,
    queue: VecDeque<(usize, i32)>,
    fault: Option<String>,
}
impl<T: Transport> Tilter<T> {
    pub fn new(mut transport: T) -> Result<Self> {
        let telemetry = transport.read()?;
        Ok(Self {
            transport,
            telemetry,
            active: None,
            queue: VecDeque::new(),
            fault: None,
        })
    }
    pub fn firmware(&self) -> &str {
        &self.telemetry.firmware
    }
    pub fn pending(&self) -> bool {
        self.active.is_some()
    }
    fn poison(&mut self, error: &anyhow::Error) {
        self.fault = Some(format!(
            "{error:#}; reconnect and inspect positions before moving again"
        ));
        self.queue.clear();
    }
    fn next(&mut self) -> Result<()> {
        if let Some((point, target)) = self.queue.pop_front() {
            // Mark pending before writing: a write failure can still have moved hardware.
            self.active = Some(Motion {
                point,
                target,
                started: Instant::now(),
                settled: 0,
            });
            if let Err(e) = self.transport.send(point, target) {
                self.poison(&e);
                return Err(e);
            }
        }
        Ok(())
    }
    pub fn status(&mut self) -> Result<Status> {
        if self.fault.is_none() {
            match self.transport.read() {
                Ok(t) => self.telemetry = t,
                Err(e) => {
                    self.poison(&e);
                    return Err(e);
                }
            }
            if let Some(m) = &mut self.active {
                if m.started.elapsed() > Duration::from_secs(60) {
                    self.poison(&anyhow::anyhow!(
                        "Point movement timed out; physical motion is unknown"
                    ));
                } else {
                    if (self.telemetry.points_um[m.point - 1] - m.target as f64).abs()
                        <= TOLERANCE_UM
                    {
                        m.settled = m.settled.saturating_add(1);
                    } else {
                        m.settled = 0;
                    }
                    if m.settled >= 3 && m.started.elapsed() >= Duration::from_millis(500) {
                        self.active = None;
                        self.next()?;
                    }
                }
            }
        }
        Ok(Status {
            position: (self.telemetry.points_um.iter().sum::<f64>() / 3.0)
                .round()
                .clamp(0.0, MAX_POSITION as f64) as i32,
            points_um: self.telemetry.points_um,
            moving: self.pending(),
            max_step: MAX_POSITION,
            tolerance_um: TOLERANCE_UM,
            halt_supported: false,
            firmware: self.telemetry.firmware.clone(),
            extra: self.telemetry.extra.clone(),
            error: if self.fault.is_some() { 1 } else { 0 },
            fault: self.fault.clone(),
        })
    }
    fn ready(&mut self) -> Result<()> {
        ensure!(
            self.fault.is_none(),
            "Session faulted; reconnect before moving"
        );
        ensure!(!self.pending(), "An ETA move is already in progress");
        self.status()?;
        Ok(())
    }
    pub fn move_point(&mut self, point: usize, target: i32) -> Result<()> {
        command(point, target)?;
        self.ready()?;
        self.queue.push_back((point, target));
        self.next()
    }
    /// Shift all points equally, retaining tilt. Validate the entire move before any write.
    pub fn move_to(&mut self, position: i32) -> Result<()> {
        ensure!(
            (0..=MAX_POSITION).contains(&position),
            "Back focus must be 0..1200 micrometres"
        );
        self.ready()?;
        let mean = self.telemetry.points_um.iter().sum::<f64>() / 3.0;
        let targets = self
            .telemetry
            .points_um
            .map(|p| (p + position as f64 - mean).round() as i32);
        for (i, target) in targets.iter().enumerate() {
            command(i + 1, *target)
                .context("Back-focus move would exceed a point limit while preserving tilt")?;
        }
        self.queue
            .extend(targets.into_iter().enumerate().map(|(i, p)| (i + 1, p)));
        self.next()
    }
    /// Only cancel points that have not started. The active motor cannot be stopped.
    pub fn cancel_queued(&mut self) {
        self.queue.clear();
    }
    pub fn halt(&mut self) -> Result<()> {
        bail!(
            "ETA has no documented stop command; use cancel-queued to discard unstarted points only"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_m54_frame_and_old_firmware() {
        let t = parse("WandererTilterM54A20260804A-0.004A-0.003A-0.005A1A\r\n").unwrap();
        assert_eq!(t.points_um, [-4., -3., -5.]);
        assert_eq!(t.extra, ["1"]);
        assert!(parse("WandererTilterM54A20250318A0A0.5A1.2A").is_ok());
        for f in [
            "WandererTilterM92A1A0A0A0A",
            "WandererTilterM54A1ANaNA0A0A",
            "WandererTilterM54A1A0A0A9A",
            "WandererTilterM54A1A0A0A0",
        ] {
            assert!(parse(f).is_err());
        }
    }
    #[test]
    fn wire_and_bounds() {
        assert_eq!(command(3, 1125).unwrap(), "31.125\n");
        assert_eq!(command(1, 10).unwrap(), "10.010\n");
        for (p, t) in [(0, 100), (4, 100), (1, -1), (2, 1201)] {
            assert!(command(p, t).is_err());
        }
    }
    #[test]
    fn preflight_and_sequential_moves() {
        let mut d = Tilter::new(simulation::Simulation::default()).unwrap();
        assert!(d.move_to(0).is_err());
        assert!(d.transport.sent.is_empty());
        d.move_to(500).unwrap();
        assert_eq!(d.transport.sent, [(1, 490)]);
        assert!(d.move_point(3, 20).is_err());
        for _ in 0..3 {
            d.active.as_mut().unwrap().started -= Duration::from_secs(1);
            d.status().unwrap();
        }
        assert_eq!(d.transport.sent, [(1, 490), (2, 500)]);
        d.cancel_queued();
        for _ in 0..3 {
            if let Some(a) = d.active.as_mut() {
                a.started -= Duration::from_secs(1);
            }
            d.status().unwrap();
        }
        assert!(!d.pending());
        assert_eq!(d.transport.sent.len(), 2);
        assert!(d.halt().is_err());
    }
    #[test]
    fn fault_never_replays_or_advances() {
        let mut d = Tilter::new(simulation::Simulation::default()).unwrap();
        d.move_to(600).unwrap();
        d.active.as_mut().unwrap().started -= Duration::from_secs(61);
        assert!(d.status().unwrap().fault.is_some());
        assert!(d.move_to(700).is_err());
        d.status().unwrap();
        assert_eq!(d.transport.sent.len(), 1);
    }
    #[test]
    fn uncertain_io_latches_fault_without_replaying_a_move() {
        struct Failing {
            inner: simulation::Simulation,
            fail_read: bool,
            fail_write: bool,
        }
        impl Transport for Failing {
            fn read(&mut self) -> Result<Telemetry> {
                ensure!(!self.fail_read, "USB disconnected");
                self.inner.read()
            }
            fn send(&mut self, point: usize, target: i32) -> Result<()> {
                self.inner.send(point, target)?;
                ensure!(!self.fail_write, "Partial write; command may have arrived");
                Ok(())
            }
        }
        for fail_write in [true, false] {
            let mut d = Tilter::new(Failing {
                inner: simulation::Simulation::default(),
                fail_read: false,
                fail_write,
            })
            .unwrap();
            assert_eq!(d.move_to(500).is_err(), fail_write);
            d.transport.fail_read = true;
            let _ = d.status();
            let status = d.status().unwrap();
            assert!(status.fault.is_some());
            assert!(status.moving); // Hardware may still be moving; never claim idle.
            assert!(d.move_to(600).is_err());
            assert_eq!(d.transport.inner.sent, [(1, 490)]);
        }
    }
}

/// Command-line worker used by regain-device.
pub mod worker;
