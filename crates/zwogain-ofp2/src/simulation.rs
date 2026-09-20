//! Explicit protocol simulator. Never selected as a hardware fallback.
use crate::Transport;
use anyhow::{Result, bail};
use std::time::{Duration, Instant};

pub const SERIAL: &str = "SIM-OFP2";
pub struct Simulation {
    cover: u8,
    target: u8,
    end: Option<Instant>,
    brightness: u16,
    light: bool,
}
impl Default for Simulation {
    fn default() -> Self {
        Self {
            cover: 1,
            target: 1,
            end: None,
            brightness: 0,
            light: false,
        }
    }
}
impl Transport for Simulation {
    fn exchange(&mut self, command: &str) -> Result<String> {
        if self.end.is_some_and(|t| Instant::now() >= t) {
            self.cover = self.target;
            self.end = None;
        }
        Ok(match command {
            "GFRM" => "Board=DeepSkyDad.FP2, Version=SIMULATION".into(),
            "GPRD" => "3".into(),
            "GOPS" => self.cover.to_string(),
            "GPOS" => match self.cover {
                0 => "270",
                1 => "0",
                _ => "135",
            }
            .into(),
            "GLON" => u8::from(self.light && self.brightness != 0).to_string(),
            "GLBR" => self.brightness.to_string(),
            "STRG0" => {
                self.target = 1;
                "OK".into()
            }
            "STRG270" => {
                self.target = 0;
                "OK".into()
            }
            "SMOV" => {
                self.cover = 2;
                self.end = Some(Instant::now() + Duration::from_secs(3));
                "OK".into()
            }
            "STOP" => {
                if self.cover == 2 {
                    self.cover = 3;
                }
                self.end = None;
                "OK".into()
            }
            "SLON0" => {
                self.light = false;
                "OK".into()
            }
            "SLON1" => {
                self.light = true;
                "OK".into()
            }
            v if v.starts_with("SLBR") => {
                self.brightness = v[4..].parse()?;
                "OK".into()
            }
            _ => bail!("Unknown simulated command {command}"),
        })
    }
}
