//! Serialized motion coordinator shared by every frontend. No motion retries.
use crate::caa::{Caa, Transport};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

struct Motion {
    target: f64,
    previous: f64,
    start: f64,
    deadline: Instant,
    remaining: f64,
    segmented: bool,
}

pub struct Controller<T: Transport> {
    pub camera: Caa<T>,
    motion: Option<Motion>,
    error: Option<String>,
    target_logical: f64,
}

impl<T: Transport> Controller<T> {
    pub fn new(mut camera: Caa<T>) -> Result<Self> {
        let target_logical = camera.status()?.logical_degrees;
        Ok(Self {
            camera,
            motion: None,
            error: None,
            target_logical,
        })
    }
    pub fn tick(&mut self) {
        if self.motion.is_none() {
            return;
        }
        if let Err(error) = self.advance() {
            self.motion = None;
            let stop = self.camera.stop();
            self.error = Some(match stop {
                Ok(()) => format!("{error:#}; stopped, movement was not retried"),
                Err(stop) => format!("{error:#}; stop also failed: {stop:#}"),
            });
            eprintln!("CAA motion: {}", self.error.as_deref().unwrap_or("failed"));
        }
    }
    fn advance(&mut self) -> Result<()> {
        let state = self.camera.status()?;
        let m = self.motion.as_mut().context("no active motion")?;
        ensure!(state.error == 0, "CAA fault {}", state.error);
        let p = state.mechanical_degrees;
        ensure!(
            p >= m.start.min(m.target) - 0.1 && p <= m.start.max(m.target) + 0.1,
            "position outside commanded travel"
        );
        let sign = (m.target - m.start).signum();
        ensure!(
            (p - m.previous) * sign >= -0.1,
            "unexpected direction of travel"
        );
        m.previous = p;
        ensure!(Instant::now() <= m.deadline, "motion deadline exceeded");
        if !state.moving {
            ensure!(
                (p - m.target).abs() <= 0.1,
                "stopped before reaching target"
            );
            let remaining = m.remaining;
            let segmented = m.segmented;
            self.motion = None;
            if segmented && remaining.abs() > 0.001 {
                self.segment(remaining)?;
            }
        }
        Ok(())
    }
    fn ready(&mut self) -> Result<()> {
        ensure!(
            self.error.is_none(),
            "acknowledge the motion error with Halt before moving again"
        );
        ensure!(self.motion.is_none(), "CAA is moving");
        let s = self.camera.status()?;
        ensure!(
            !s.moving && s.error == 0,
            "CAA is moving or reports a fault"
        );
        Ok(())
    }
    fn segment(&mut self, remaining: f64) -> Result<()> {
        let amount = remaining.abs().min(90.0);
        let (start, target) = if remaining > 0.0 {
            (0.0, amount)
        } else {
            (amount, 0.0)
        };
        self.camera.set_mechanical_reference(start)?;
        self.camera.move_mechanical(target)?;
        self.motion = Some(Motion {
            start,
            previous: start,
            target,
            remaining: remaining - remaining.signum() * amount,
            segmented: true,
            deadline: Instant::now() + Duration::from_secs(30),
        });
        Ok(())
    }
    pub fn halt(&mut self) -> Result<()> {
        self.motion = None;
        self.camera.stop()?;
        self.error = None;
        Ok(())
    }
    pub fn request(&mut self, v: &Value) -> Result<Value> {
        let command = v["command"].as_str().context("command required")?;
        // Halt must discard the pending segments before any tick can start one.
        if command != "stop" {
            self.tick();
        }
        let degrees = || {
            v["degrees"]
                .as_f64()
                .filter(|v| v.is_finite())
                .context("finite degrees required")
        };
        let enabled = || v["enabled"].as_bool().context("enabled must be boolean");
        match command {
            "status" => {
                let mut s = serde_json::to_value(self.camera.status()?)?;
                s["moving"] = json!(s["moving"] == true || self.motion.is_some());
                s["motion_error"] = json!(self.error);
                s["target_degrees"] = json!(self.target_logical);
                return Ok(s);
            }
            "identity" => return Ok(serde_json::to_value(self.camera.identity()?)?),
            "settings" => return Ok(serde_json::to_value(self.camera.settings()?)?),
            "stop" => self.halt()?,
            other => {
                self.ready()?;
                match other {
                    "rotate-unwrapped" => {
                        let amount = degrees()?;
                        ensure!(
                            amount != 0.0 && amount.abs() <= 450.0,
                            "explicit travel must be within -450..450 degrees and nonzero"
                        );
                        let s = self.camera.status()?;
                        ensure!(
                            s.limit_degrees >= 90,
                            "multi-turn travel requires a limit of at least 90 degrees"
                        );
                        let sign = if self.camera.settings()?.reverse {
                            -1.0
                        } else {
                            1.0
                        };
                        self.target_logical = (s.logical_degrees + sign * amount).rem_euclid(360.0);
                        // If a start write is uncertain, stop and report it, never replay.
                        if let Err(e) = self.segment(amount) {
                            let _ = self.camera.stop();
                            self.error = Some(format!("{e:#}"));
                            return Err(e);
                        }
                        eprintln!(
                            "CAA explicit travel {amount} degrees; resets the mechanical reference"
                        );
                    }
                    "move-mechanical" | "move-to" | "move-relative" => {
                        let d = degrees()?;
                        let s = self.camera.status()?;
                        let reverse = self.camera.settings()?.reverse;
                        let sign = if reverse { -1.0 } else { 1.0 };
                        let target = match other {
                            "move-mechanical" => d,
                            "move-to" => {
                                ensure!((0.0..=360.0).contains(&d), "logical angle must be 0..360");
                                super::wrap_mechanical((d - s.logical_offset) * sign)
                            }
                            _ => {
                                ensure!(d.abs() <= 360.0, "relative angle must be within one turn");
                                super::wrap_mechanical(s.mechanical_degrees + sign * d)
                            }
                        };
                        super::mechanical_angle(target)?;
                        ensure!(
                            target <= f64::from(s.limit_degrees),
                            "target exceeds rotation limit"
                        );
                        if let Err(e) = self.camera.move_mechanical(target) {
                            let _ = self.camera.stop();
                            self.error = Some(format!("{e:#}; motion was not retried"));
                            return Err(e);
                        }
                        self.target_logical = (sign * target + s.logical_offset).rem_euclid(360.0);
                        self.motion = Some(Motion {
                            target,
                            previous: s.mechanical_degrees,
                            start: s.mechanical_degrees,
                            deadline: Instant::now() + Duration::from_secs(120),
                            remaining: 0.0,
                            segmented: false,
                        });
                    }
                    "sync" => self.camera.sync(degrees()?)?,
                    "reference" => {
                        self.camera.set_mechanical_reference(degrees()?)?;
                        eprintln!("CAA mechanical reference changed explicitly");
                    }
                    "reset-origin" => {
                        self.camera.set_mechanical_reference(0.0)?;
                        eprintln!("CAA mechanical origin explicitly reset to zero");
                    }
                    "beep" => self.camera.set_beep(enabled()?)?,
                    "reverse" => self.camera.set_reverse(enabled()?)?,
                    "alias" => self
                        .camera
                        .set_alias(v["text"].as_str().context("text required")?)?,
                    "limit" => self.camera.set_limit(
                        v["degrees"]
                            .as_u64()
                            .and_then(|n| u16::try_from(n).ok())
                            .context("whole-degree limit required")?,
                    )?,
                    _ => bail!("unknown command {other}"),
                }
            }
        }
        Ok(json!({"accepted":true}))
    }
}
