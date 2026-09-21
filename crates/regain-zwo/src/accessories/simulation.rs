//! Explicit opt-in protocol simulator; never selected as a hardware fallback.
use crate::accessories::{Kind, Transport};
use anyhow::{Result, ensure};
pub struct Sim {
    kind: Kind,
    selector: u8,
    status: [u8; 16],
    calibration_started: Option<std::time::Instant>,
}
impl Sim {
    pub fn new(kind: Kind) -> Self {
        let status = match kind {
            Kind::Efw => [1, 126, 90, 1, 1, 0, 1, 1, 1, 7, 0, 0, 0, 0, 7, 0],
            Kind::Eaf => [
                1, 126, 90, 3, 0, 0, 0, 0, 0x9b, 0x95, 0, 0x80, 0x52, 1, 0xea, 0x60,
            ],
        };
        Self {
            kind,
            selector: 0,
            status,
            calibration_started: None,
        }
    }
}
impl Transport for Sim {
    fn set_output(&mut self, b: &[u8]) -> Result<()> {
        ensure!(
            b.len() >= 16 && b[..3] == [3, 126, 90],
            "invalid simulated output"
        );
        if b[3] == 2 {
            self.selector = b[4];
        } else if self.kind == Kind::Efw && b[3] == 1 {
            if b[4] == 1 {
                self.calibration_started = Some(std::time::Instant::now());
            } else {
                self.status[6..9].fill(b[5]);
            }
        } else if self.kind == Kind::Eaf && b[3] == 3 {
            self.status[5..10].copy_from_slice(&b[5..10]);
            self.status[13..16].copy_from_slice(&b[13..16]);
        } else {
            anyhow::bail!("unsupported simulated command");
        }
        Ok(())
    }
    fn get_input(&mut self) -> Result<Vec<u8>> {
        if self.selector == if self.kind == Kind::Efw { 1 } else { 3 } {
            if let Some(started) = self.calibration_started {
                if started.elapsed() < std::time::Duration::from_secs(2) {
                    let mut status = self.status;
                    status[4] = 0;
                    status[9] = 1 + (started.elapsed().as_millis() / 300).min(6) as u8;
                    return Ok(status.to_vec());
                }
                self.calibration_started = None;
                self.status[6..9].fill(1);
            }
            return Ok(self.status.to_vec());
        }
        let mut b = vec![0; 16];
        b[..4].copy_from_slice(&[1, 126, 90, self.selector]);
        match self.selector {
            4 => {
                b[4..7].copy_from_slice(&if self.kind == Kind::Efw {
                    [3, 6, 2]
                } else {
                    [3, 8, 1]
                });
                let name = if self.kind == Kind::Efw {
                    b"EFW-S-0".as_slice()
                } else {
                    b"EAFN".as_slice()
                };
                b[8..8 + name.len()].copy_from_slice(name);
            }
            12 => b[4..12].copy_from_slice(&[
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                if self.kind == Kind::Efw { 8 } else { 9 },
            ]),
            _ => anyhow::bail!("unsupported simulated query"),
        }
        Ok(b)
    }
}
