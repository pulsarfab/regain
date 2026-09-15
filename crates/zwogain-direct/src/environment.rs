//! ASI2600/6200 environment controls, serviced on the transport owner thread.
//! Register mapping/current calibration observed with SDK 1.41, PIDs 2601/620b.
//! The regulator is our own bounded PI controller, not the SDK's PID algorithm.
use crate::transport::Camera;
use anyhow::{Result, ensure};
use std::time::Instant;

const CURRENT: [(f64, f64); 12] = [
    (0.0, 255.0),
    (1.31, 220.0),
    (1.94, 200.0),
    (2.53, 180.0),
    (3.15, 160.0),
    (3.7, 140.0),
    (4.2, 120.0),
    (4.7, 100.0),
    (5.2, 80.0),
    (5.6, 60.0),
    (5.85, 50.0),
    (6.01, 40.0),
];

pub fn power_register(percent: f64) -> u16 {
    let amps = percent.clamp(0.0, 100.0) * 6.01 / 100.0;
    let mut dac = 40;
    for pair in CURRENT.windows(2) {
        let [(a, x), (b, y)] = pair else {
            unreachable!()
        };
        if amps <= *b {
            dac = (x + (amps - a) / (b - a) * (y - x)) as u16;
            break;
        }
    }
    (272 - dac.clamp(40, 255)) * 220 / 256
}

pub struct Environment {
    pub target: i64,
    pub enabled: bool,
    pub dew: bool,
    pub temperature: f64,
    pub power: f64,
    integral: f64,
    tick: Instant,
    auxiliary: Option<(i64, i64)>,
}

fn flags(camera: &Camera, mask: u8, enabled: bool) -> Result<()> {
    let old = camera.vendor(0xbc, 0x19, 0, 1)?[0];
    camera.vendor(
        0xbd,
        0x19,
        u16::from(if enabled { old | mask } else { old & !mask }),
        0,
    )?;
    Ok(())
}

impl Environment {
    pub fn open(camera: &Camera, auxiliary: bool) -> Result<Self> {
        let bits = camera.vendor(0xbc, 0x19, 0, 1)?[0];
        let register = camera.vendor(0xbc, 0x26, 0, 1)?[0];
        let temperature = Self::read_temperature(camera)?;
        // Match the observed output on reconnect before the plugin restores its target.
        let power = if bits & 0x80 != 0 {
            0.0
        } else {
            (0..=100)
                .min_by_key(|&p| power_register(f64::from(p)).abs_diff(u16::from(register)))
                .unwrap() as f64
        };
        Ok(Self {
            target: (temperature.round() as i64).clamp(-40, 30),
            enabled: bits & 0x80 == 0,
            dew: bits & 0x40 != 0,
            temperature,
            power,
            integral: power,
            tick: Instant::now(),
            auxiliary: if auxiliary {
                Some((
                    i64::from(camera.vendor(0xbc, 0xfa, 0, 1)?[0]),
                    i64::from(camera.vendor(0xbc, 0xfb, 0, 1)?[0]),
                ))
            } else {
                None
            },
        })
    }
    fn read_temperature(camera: &Camera) -> Result<f64> {
        let bytes = camera.vendor(0xb3, 0, 0, 2)?;
        let temperature = f64::from(i16::from_le_bytes(bytes.try_into().unwrap())) / 256.0;
        ensure!(
            (-50.0..=85.0).contains(&temperature),
            "invalid camera temperature {temperature}"
        );
        Ok(temperature)
    }
    pub fn get(&self, control: u32) -> Result<i64> {
        Ok(match control {
            8 => (self.temperature * 10.0) as i64,
            15 => self.power.round() as i64,
            16 => self.target,
            17 => i64::from(self.enabled),
            21 => i64::from(self.dew),
            22 if self.auxiliary.is_some() => self.auxiliary.unwrap().0,
            23 if self.auxiliary.is_some() => self.auxiliary.unwrap().1,
            _ => anyhow::bail!("unsupported environment control"),
        })
    }
    pub fn set(&mut self, camera: &Camera, control: u32, value: i64) -> Result<()> {
        match control {
            16 => {
                ensure!((-40..=30).contains(&value), "invalid cooler target");
                self.target = value;
            }
            17 => {
                ensure!((0..=1).contains(&value), "invalid cooler enable");
                if value == 0 || !self.enabled {
                    camera.vendor(0xbd, 0x26, power_register(0.0), 0)?;
                    self.power = 0.0;
                    self.integral = 0.0;
                }
                flags(camera, 0x80, value == 0)?;
                self.enabled = value != 0;
            }
            21 => {
                ensure!((0..=1).contains(&value), "invalid dew heater enable");
                camera.vendor(0xbd, 0x2a, if value == 0 { 0 } else { 197 }, 0)?;
                flags(camera, 0x40, value != 0)?;
                self.dew = value != 0;
            }
            22 | 23 if self.auxiliary.is_some() => {
                ensure!((0..=255).contains(&value), "fan/LED value must be 0..255");
                let register = if control == 22 { 0xfa } else { 0xfb };
                camera.vendor(0xbd, register, value as u16, 0)?;
                let actual = i64::from(camera.vendor(0xbc, register, 0, 1)?[0]);
                ensure!(
                    actual == value,
                    "fan/LED register {register:#x}: wrote {value}, read {actual}"
                );
                let values = self.auxiliary.as_mut().unwrap();
                if control == 22 {
                    values.0 = value;
                } else {
                    values.1 = value;
                }
            }
            _ => anyhow::bail!("environment control is read-only or unsupported"),
        }
        Ok(())
    }
    pub fn service(&mut self, camera: &Camera) -> Result<()> {
        let elapsed = self.tick.elapsed().as_secs_f64();
        if elapsed < 1.0 {
            return Ok(());
        }
        self.tick = Instant::now();
        self.temperature = match Self::read_temperature(camera) {
            Ok(t) => t,
            Err(error) => {
                // Stop active cooling when temperature feedback is unavailable.
                let _ = self.set(camera, 17, 0);
                return Err(error);
            }
        };
        if self.enabled {
            // Limit elapsed time after blocked USB I/O: no accumulated power jump.
            let dt = elapsed.min(2.0);
            let error = self.temperature - self.target as f64;
            self.integral = (self.integral + error * 0.08 * dt).clamp(0.0, 100.0);
            let demand = (self.integral + 2.0 * error).clamp(0.0, 100.0);
            self.power = demand.clamp(
                (self.power - 2.0 * dt).max(0.0),
                (self.power + 2.0 * dt).min(100.0),
            );
            camera.vendor(0xbd, 0x26, power_register(self.power), 0)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observed_current_conversion_is_bounded_and_monotonic() {
        assert_eq!(power_register(0.0), 14);
        assert_eq!(power_register(1.0), 16);
        assert_eq!(power_register(100.0), 199);
        for p in 0..100 {
            assert!(power_register(f64::from(p)) <= power_register(f64::from(p + 1)));
        }
    }
}
