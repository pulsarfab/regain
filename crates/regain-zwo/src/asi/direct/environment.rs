//! ASI585/2600/6200 environment controls, serviced on the transport owner thread.
//! Register mapping/current calibration observed with SDK 1.41, PIDs 2601/620b.
//! The regulator is our own bounded PI controller, not the SDK's PID algorithm.
use crate::asi::direct::transport::Camera;
use anyhow::{Result, ensure};
use std::time::Instant;
mod control;
pub(super) use control::{CoolingError, CoolingQueue};

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

fn power_dac(percent: f64) -> u16 {
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
    dac.clamp(40, 255)
}

pub fn power_register(percent: f64) -> u16 {
    (272 - power_dac(percent)) * 220 / 256
}

#[derive(Clone, Copy, PartialEq)]
pub enum CoolerOutput {
    Fpga,
    Dac,
}

const PROPORTIONAL: f64 = 8.0;
const INTEGRAL: f64 = 0.12;

pub(super) fn recovery_state(
    power: f64,
    prior: f64,
    current: f64,
    target: f64,
    previous_target: f64,
) -> (f64, f64) {
    if target > previous_target {
        return (0.0, 0.0);
    }
    let integral = (power - PROPORTIONAL * (prior - previous_target)).clamp(0.0, 100.0);
    let boosted = (power + PROPORTIONAL * (current - prior + previous_target - target).max(0.0))
        .clamp(0.0, 100.0);
    (boosted, integral)
}

fn target_change(
    power: f64,
    integral: f64,
    temperature: f64,
    previous: f64,
    target: f64,
) -> (f64, f64) {
    if temperature <= target {
        return (0.0, 0.0);
    }
    let demand = (integral + PROPORTIONAL * (temperature - target)).clamp(0.0, 100.0);
    // A deliberate setpoint change gets its proportional response immediately.
    // Normal feedback still uses the bounded slew and anti-windup controller.
    (
        if target < previous {
            power.max(demand)
        } else {
            power.min(demand)
        },
        integral,
    )
}

// Percentage points per second, with bounded elapsed time after blocked USB.
fn regulate(power: f64, integral: &mut f64, error: f64, elapsed: f64) -> f64 {
    let dt = elapsed.clamp(0.0, 2.0);
    let candidate = (*integral + error * INTEGRAL * dt).clamp(0.0, 100.0);
    let demand = candidate + PROPORTIONAL * error;
    // Do not accumulate demand the actuator cannot deliver. Allow unwinding.
    if (demand <= 100.0 || error < 0.0) && (demand >= 0.0 || error > 0.0) {
        *integral = candidate;
    }
    (*integral + PROPORTIONAL * error)
        .clamp(0.0, 100.0)
        .clamp((power - 12.0 * dt).max(0.0), (power + 8.0 * dt).min(100.0))
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
    heater: bool,
    output: CoolerOutput,
    observed_at: [Instant; 4],
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
    /// Resume the last measured demand after worker replacement. The remembered
    /// temperature separates the old proportional demand from its steady load.
    /// A warmer requested target must never reinstate unwanted cooling.
    pub fn resume_cooling(
        &mut self,
        camera: &Camera,
        power: i64,
        prior: f64,
        previous_target: i64,
    ) -> Result<()> {
        ensure!(
            (0..=100).contains(&power)
                && prior.is_finite()
                && (-50.0..=85.0).contains(&prior)
                && (-40..=30).contains(&previous_target),
            "invalid cooling recovery sample"
        );
        ensure!(self.enabled, "cooler is disabled");
        self.temperature = match Self::read_temperature(camera) {
            Ok(t) => t,
            Err(error) => {
                let _ = self.set(camera, 17, 0);
                return Err(error);
            }
        };
        let (power, integral) = recovery_state(
            power as f64,
            prior,
            self.temperature,
            self.target as f64,
            previous_target as f64,
        );
        self.write_power(camera, power)?;
        self.power = power;
        self.integral = integral;
        self.tick = Instant::now();
        self.observed_at[0] = self.tick;
        self.observed_at[1] = self.tick;
        crate::asi::direct::diagnostics::log(
            "info",
            "cooling.resumed",
            format_args!(
                "Resumed output {power:.1}% at {:.2} C (prior {prior:.2} C), target {} C",
                self.temperature, self.target
            ),
        );
        Ok(())
    }
    /// Restore actuator state after a handle reconnect without replacing the
    /// saved setpoint, regulator history, or the frame retained in DDR.
    pub fn restore(&mut self, camera: &Camera) -> Result<()> {
        let observed = Instant::now();
        self.write_power(camera, if self.enabled { self.power } else { 0.0 })?;
        flags(camera, 0x80, !self.enabled)?;
        if self.heater {
            camera.vendor(0xbd, 0x2a, if self.dew { 197 } else { 0 }, 0)?;
            flags(camera, 0x40, self.dew)?;
        }
        if let Some((fan, led)) = self.auxiliary {
            self.set(camera, 22, fan)?;
            self.set(camera, 23, led)?;
        }
        let expected_flags = (u8::from(!self.enabled) * 0x80) | (u8::from(self.dew) * 0x40);
        ensure!(
            camera.vendor(0xbc, 0x19, 0, 1)?[0] & if self.heater { 0xc0 } else { 0x80 }
                == expected_flags,
            "cooler/dew state did not restore"
        );
        if self.output == CoolerOutput::Fpga {
            ensure!(
                u16::from(camera.vendor(0xbc, 0x26, 0, 1)?[0])
                    == power_register(if self.enabled { self.power } else { 0.0 }),
                "cooler output did not restore"
            );
        }
        self.temperature = Self::read_temperature(camera)?;
        for index in [0, 1, 3] {
            self.observed_at[index] = observed;
        }
        self.tick = Instant::now();
        Ok(())
    }
    pub fn open(
        camera: &Camera,
        auxiliary: bool,
        heater: bool,
        output: CoolerOutput,
    ) -> Result<Self> {
        let observed = Instant::now();
        let bits = camera.vendor(0xbc, 0x19, 0, 1)?[0];
        // The ASI585 DAC has no traced output readback. Establish zero demand
        // on a fresh connection while preserving the cooler enable bit.
        let register = if output == CoolerOutput::Fpga {
            camera.vendor(0xbc, 0x26, 0, 1)?[0]
        } else {
            camera.vendor(0xb2, power_dac(0.0), 0, 0)?;
            0
        };
        let temperature = Self::read_temperature(camera)?;
        // Match the observed output on reconnect before the plugin restores its target.
        let power = if bits & 0x80 != 0 || output == CoolerOutput::Dac {
            0.0
        } else {
            (0..=100)
                .min_by_key(|&p| power_register(f64::from(p)).abs_diff(u16::from(register)))
                .unwrap() as f64
        };
        Ok(Self {
            target: (temperature.round() as i64).clamp(-40, 30),
            enabled: bits & 0x80 == 0,
            dew: heater && bits & 0x40 != 0,
            heater,
            output,
            observed_at: [observed; 4],
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
    fn write_power(&self, camera: &Camera, percent: f64) -> Result<()> {
        match self.output {
            CoolerOutput::Fpga => camera.vendor(0xbd, 0x26, power_register(percent), 0)?,
            CoolerOutput::Dac => camera.vendor(0xb2, power_dac(percent), 0, 0)?,
        };
        Ok(())
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
            21 if self.heater => i64::from(self.dew),
            22 if self.auxiliary.is_some() => self.auxiliary.unwrap().0,
            23 if self.auxiliary.is_some() => self.auxiliary.unwrap().1,
            _ => anyhow::bail!("unsupported environment control"),
        })
    }
    pub fn observed_at(&self) -> [Instant; 4] {
        self.observed_at
    }
    pub fn set(&mut self, camera: &Camera, control: u32, value: i64) -> Result<()> {
        let observed = Instant::now();
        match control {
            16 => {
                ensure!((-40..=30).contains(&value), "invalid cooler target");
                if self.enabled && value != self.target {
                    let temperature = self.feedback(camera)?;
                    let (power, integral) = target_change(
                        self.power,
                        self.integral,
                        temperature,
                        self.target as f64,
                        value as f64,
                    );
                    self.write_power(camera, power)?;
                    self.power = power;
                    self.integral = integral;
                    self.temperature = temperature;
                    self.tick = observed;
                    self.observed_at[0] = observed;
                    self.observed_at[1] = observed;
                    crate::asi::direct::diagnostics::log(
                        "info",
                        "cooling.target_changed",
                        format_args!(
                            "Target {} -> {value} C; output {power:.1}% at {temperature:.2} C",
                            self.target
                        ),
                    );
                }
                self.target = value;
                self.observed_at[2] = observed;
            }
            17 => {
                ensure!((0..=1).contains(&value), "invalid cooler enable");
                if value == 0 || !self.enabled {
                    let power = if value == 0 {
                        0.0
                    } else {
                        self.temperature = self.feedback(camera)?;
                        self.observed_at[0] = observed;
                        (PROPORTIONAL * (self.temperature - self.target as f64)).clamp(0.0, 100.0)
                    };
                    self.write_power(camera, power)?;
                    self.power = power;
                    self.tick = observed;
                    self.observed_at[1] = observed;
                    self.integral = 0.0;
                }
                flags(camera, 0x80, value == 0)?;
                self.enabled = value != 0;
                self.observed_at[3] = observed;
            }
            21 if self.heater => {
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
    fn feedback(&mut self, camera: &Camera) -> Result<f64> {
        match Self::read_temperature(camera) {
            Ok(t) => Ok(t),
            Err(error) => {
                let _ = self.set(camera, 17, 0);
                Err(error)
            }
        }
    }
    pub fn service(&mut self, camera: &Camera) -> Result<()> {
        let elapsed = self.tick.elapsed().as_secs_f64();
        if elapsed < 1.0 {
            return Ok(());
        }
        self.tick = Instant::now();
        let observed = self.tick;
        self.temperature = match Self::read_temperature(camera) {
            Ok(t) => t,
            Err(error) => {
                // Stop active cooling when temperature feedback is unavailable.
                let _ = self.set(camera, 17, 0);
                return Err(error);
            }
        };
        self.observed_at[0] = observed;
        if self.enabled {
            // Limit elapsed time after blocked USB I/O: no accumulated power jump.
            let error = self.temperature - self.target as f64;
            self.power = regulate(self.power, &mut self.integral, error, elapsed);
            self.write_power(camera, self.power)?;
            self.observed_at[1] = observed;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_restores_previous_output_and_boosts_for_warming() {
        assert_eq!(
            recovery_state(40.0, -10.0, -10.0, -10.0, -10.0),
            (40.0, 40.0)
        );
        assert_eq!(
            recovery_state(40.0, -10.0, -8.0, -10.0, -10.0),
            (56.0, 40.0)
        );
        assert_eq!(
            recovery_state(90.0, -10.0, -5.0, -10.0, -10.0),
            (100.0, 90.0)
        );
        assert_eq!(recovery_state(40.0, -10.0, -9.0, -9.0, -10.0), (0.0, 0.0));
        assert_eq!(
            recovery_state(40.0, -10.0, -8.0, -15.0, -10.0),
            (96.0, 40.0)
        );
        let (power, mut integral) = recovery_state(40.0, -10.0, -8.0, -10.0, -10.0);
        assert!(regulate(power, &mut integral, 2.0, 1.0) >= power);
    }
    #[test]
    fn changed_target_converges_promptly_and_releases_unwanted_cooling() {
        assert_eq!(target_change(40.0, 40.0, -10.0, -10.0, -15.0), (80.0, 40.0));
        assert_eq!(
            target_change(40.0, 40.0, -10.0, -10.0, -30.0),
            (100.0, 40.0)
        );
        assert_eq!(target_change(96.0, 40.0, -8.0, -15.0, -10.0), (56.0, 40.0));
        assert_eq!(target_change(96.0, 40.0, -8.0, -15.0, -5.0), (0.0, 0.0));
        assert_eq!(target_change(0.0, 0.0, 25.0, 25.0, -10.0), (100.0, 0.0));
        assert_eq!(target_change(40.0, 40.0, -20.0, -10.0, -15.0), (0.0, 0.0));
    }
    #[test]
    fn fast_setpoint_changes_converge_without_large_overshoot() {
        for (thermal_seconds, cooling, lag_seconds) in
            [(20.0, 0.03, 5.0), (60.0, 0.01, 8.0), (180.0, 0.0025, 10.0)]
        {
            let mut temperature = 20.0;
            let mut power = 100.0;
            let mut integral = 0.0;
            let mut actuator = 0.0;
            let mut previous = 0.0;
            for target in [0.0, -5.0, 5.0] {
                if target != previous {
                    let prior_power = power;
                    (power, integral) =
                        target_change(power, integral, temperature, previous, target);
                    if target < previous {
                        assert!(power > prior_power + 20.0);
                    } else {
                        assert_eq!(power, 0.0);
                    }
                }
                for second in 0..1200 {
                    power = regulate(power, &mut integral, temperature - target, 1.0);
                    actuator += (power - actuator) / lag_seconds;
                    temperature += (20.0 - temperature) / thermal_seconds - cooling * actuator;
                    assert!((0.0..=100.0).contains(&power));
                    if target <= previous {
                        assert!(temperature > target - 2.0, "overshoot {temperature}");
                    }
                    if second > 600 {
                        assert!(
                            (temperature - target).abs() < 0.5,
                            "temperature {temperature}, target {target}"
                        );
                    }
                }
                previous = target;
            }
        }
    }
    #[test]
    fn responds_promptly_without_windup_or_unbounded_elapsed_time() {
        let mut integral = 0.0;
        let mut power = 0.0;
        for _ in 0..20 {
            power = regulate(power, &mut integral, 100.0, 1.0);
        }
        assert_eq!(power, 100.0);
        assert_eq!(integral, 0.0);
        assert_eq!(regulate(power, &mut integral, 0.0, 1.0), 88.0);
        assert_eq!(regulate(0.0, &mut integral, 100.0, 600.0), 16.0);
        assert_eq!(regulate(0.0, &mut integral, -10.0, 1.0), 0.0);
    }

    #[test]
    fn simulated_thermal_load_recovers_without_large_overshoot() {
        for (thermal_seconds, cooling, lag_seconds) in
            [(20.0, 0.03, 5.0), (60.0, 0.01, 8.0), (180.0, 0.0025, 10.0)]
        {
            let target = 0.0;
            let mut temperature = 20.0;
            let mut power = 0.0;
            let mut integral = 0.0;
            let mut actuator = 0.0;
            let mut minimum = temperature;
            for second in 0..1200 {
                power = regulate(power, &mut integral, temperature - target, 1.0);
                actuator += (power - actuator) / lag_seconds;
                temperature += (20.0 - temperature) / thermal_seconds - cooling * actuator;
                minimum = temperature.min(minimum);
                assert!((0.0..=100.0).contains(&power));
                if second > 600 {
                    assert!(
                        (temperature - target).abs() < 0.5,
                        "temperature {temperature}"
                    );
                }
            }
            assert!(minimum > target - 2.0, "overshoot {minimum}");
        }
    }
    #[test]
    fn observed_current_conversion_is_bounded_and_monotonic() {
        assert_eq!(power_dac(0.0), 255);
        assert_eq!(power_dac(100.0), 40);
        assert_eq!(power_register(0.0), 14);
        assert_eq!(power_register(1.0), 16);
        assert_eq!(power_register(100.0), 199);
        for p in 0..100 {
            assert!(power_register(f64::from(p)) <= power_register(f64::from(p + 1)));
            assert!(power_dac(f64::from(p)) >= power_dac(f64::from(p + 1)));
        }
    }

    #[test]
    fn cooler_only_model_does_not_report_heater_or_auxiliary_controls() {
        let environment = Environment {
            target: 20,
            enabled: false,
            dew: false,
            temperature: 21.5,
            power: 0.0,
            integral: 0.0,
            tick: Instant::now(),
            auxiliary: None,
            heater: false,
            output: CoolerOutput::Dac,
            observed_at: [Instant::now(); 4],
        };
        assert_eq!(environment.get(8).unwrap(), 215);
        for control in [21, 22, 23] {
            assert!(environment.get(control).is_err());
        }
    }
}
