use crate::fc3::Transport;
use anyhow::{Result, bail};
use std::time::{Duration, Instant};
pub const SERIAL: &str = "00:00:00:00:00:03";
pub struct Simulation {
    position: i32,
    target: Option<(i32, Instant)>,
    speed: u16,
    backlash: u16,
    reverse: bool,
}
impl Default for Simulation {
    fn default() -> Self {
        Self {
            position: 1000,
            target: None,
            speed: 400,
            backlash: 0,
            reverse: false,
        }
    }
}
impl Transport for Simulation {
    fn exchange(&mut self, c: &str) -> Result<String> {
        if let Some((p, t)) = self.target {
            if Instant::now() >= t {
                self.position = p;
                self.target = None;
            } else {
                self.position += (p - self.position).signum();
            }
        }
        Ok(match c {
            "F#" => "FC3_SIM".into(),
            "FV" => "0.0.0".into(),
            "FA" => format!(
                "FC3:{}:{}:22.5:{}:{}",
                self.position,
                u8::from(self.target.is_some()),
                u8::from(self.reverse),
                self.backlash
            ),
            "SP" => self.speed.to_string(),
            "FH" => {
                self.target = None;
                "1".into()
            }
            _ => {
                let (key, v) = c
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("Unknown command"))?;
                match key {
                    "FM" => {
                        self.target =
                            Some((v.parse()?, Instant::now() + Duration::from_millis(400)))
                    }
                    "SP" => self.speed = v.parse()?,
                    "BL" => self.backlash = v.parse()?,
                    "FD" => self.reverse = v == "1",
                    _ => bail!("Unknown command"),
                };
                v.into()
            }
        })
    }
}
