use crate::Transport;
use anyhow::{Result, bail};
use std::time::{Duration, Instant};
pub const SERIAL: &str = "FALCON-SIMULATION";
#[derive(Default)]
pub struct Simulation {
    position: f64,
    target: Option<(f64, Instant)>,
    reverse: bool,
}
impl Transport for Simulation {
    fn exchange(&mut self, c: &str) -> Result<String> {
        if let Some((p, t)) = self.target
            && Instant::now() >= t
        {
            self.position = p;
            self.target = None;
        }
        Ok(match c {
            "F#" => "F2R_12345678_A".into(),
            "FV" => "FV:1.8".into(),
            "FA" => format!(
                "F2R:{:.2}:{}:4500:4:{}",
                self.position,
                u8::from(self.target.is_some()),
                u8::from(self.reverse)
            ),
            "FH" => {
                self.target = None;
                "FH:1".into()
            }
            "FN:0" | "FN:1" => {
                self.reverse = c == "FN:1";
                c.into()
            }
            _ => {
                let (op, value) = c
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("Invalid simulated command"))?;
                let value: f64 = value.parse()?;
                match op {
                    "MD" => {
                        self.target = Some((value, Instant::now() + Duration::from_millis(150)))
                    }
                    "SD" => self.position = value,
                    _ => bail!("Unsupported simulated command"),
                }
                c.into()
            }
        })
    }
}
