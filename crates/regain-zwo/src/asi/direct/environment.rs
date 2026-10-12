//! ASI585/2600/6200 environment controls, serviced on the transport owner thread.
//! Register mapping/current calibration observed with SDK 1.41, PIDs 2601/620b.
//! The regulator is our own bounded PI controller, not the SDK's PID algorithm.
//! See docs/camera-usb-registers.md for request notation and evidence limits.
use crate::asi::direct::transport::Camera;
use anyhow::{Result, ensure};
use std::time::Instant;
mod control;
pub(super) use control::{CoolingError, CoolingQueue};

// SDK type-1 current/DAC lookup: (nominal amperes, DAC code). These are
// calibration points used to command power, not an electrical-current readback.
// SDK 1.41 cooler dispatch 10ec60, table 2876e0; docs/duo-capture.md.
// DAC 255 is zero demand; 40 is the SDK's 6.01 A full-demand endpoint.
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
    // SDK 10e870 converts the interpolated DAC code to FPGA BD26. Preserve
    // integer truncation/order: 0%, 1%, 100% become 14, 16, 199, not 0..255.
    // ASI585 sends the DAC code directly with B2 and bypasses this conversion.
    (272 - power_dac(percent)) * 220 / 256
}

#[derive(Clone, Copy, PartialEq)]
pub enum CoolerOutput {
    Fpga,
    Dac,
}

// Host-controller policy, not camera registers or copied SDK PID constants:
// proportional percentage points/degree C and integral points/(degree C*s).
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
    // Recompute proportional demand immediately for a deliberate setpoint
    // change. The caller applies the live power-change limit before writing it.
    (
        if target < previous {
            power.max(demand)
        } else {
            power.min(demand)
        },
        integral,
    )
}

// Percentage points per five seconds. Do not bank ramp allowance across a
// blocked USB operation; at most two seconds may be applied in one update.
fn limit_power(power: f64, demand: f64, elapsed: f64, maximum: f64) -> f64 {
    let change = maximum / 5.0 * elapsed.clamp(0.0, 2.0);
    demand
        .clamp(0.0, 100.0)
        .clamp((power - change).max(0.0), (power + change).min(100.0))
}

fn regulate(power: f64, integral: &mut f64, error: f64, elapsed: f64, maximum: f64) -> f64 {
    let dt = elapsed.clamp(0.0, 2.0);
    let candidate = (*integral + error * INTEGRAL * dt).clamp(0.0, 100.0);
    let demand = candidate + PROPORTIONAL * error;
    // Do not accumulate demand the actuator cannot deliver. Allow unwinding.
    if (demand <= 100.0 || error < 0.0) && (demand >= 0.0 || error > 0.0) {
        *integral = candidate;
    }
    limit_power(power, *integral + PROPORTIONAL * error, dt, maximum)
}

pub struct Environment {
    pub target: i64,
    pub enabled: bool,
    pub dew: bool,
    pub temperature: f64,
    pub power: f64,
    integral: f64,
    tick: Instant,
    power_tick: Instant,
    max_power_change: f64,
    pub(super) live_power_limit: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
    auxiliary: Option<(i64, i64)>,
    heater: bool,
    output: CoolerOutput,
    observed_at: [Instant; 4],
}

fn flags(camera: &Camera, mask: u8, enabled: bool) -> Result<()> {
    // BC19/BD19 is shared with exposure clocks: bit 0 gates a clock, 0x80
    // DISABLES cooling, and 0x40 enables the advertised heater. Read-modify-write
    // only the requested mask; writing an entire preset can break acquisition.
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
    pub fn set_power_limit(&mut self, maximum: f64) -> Result<()> {
        let options = regain_core::RecoveryOptions {
            cooler_max_power_change_per_5_seconds: maximum,
            ..Default::default()
        };
        options.validate()?;
        self.max_power_change = maximum;
        crate::asi::direct::diagnostics::log(
            "info",
            "cooling.power_limit",
            format_args!("Direct cooler limit {maximum} percentage points per 5 s"),
        );
        Ok(())
    }
    fn limited_power(&self, demand: f64, observed: Instant) -> f64 {
        limit_power(
            self.power,
            demand,
            observed.duration_since(self.power_tick).as_secs_f64(),
            self.power_limit(),
        )
    }
    fn power_limit(&self) -> f64 {
        self.live_power_limit
            .as_ref()
            .map_or(self.max_power_change, |value| {
                f64::from_bits(value.load(std::sync::atomic::Ordering::Relaxed))
            })
    }
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
        let remembered_power = power as f64;
        let (demand, integral) = recovery_state(
            power as f64,
            prior,
            self.temperature,
            self.target as f64,
            previous_target as f64,
        );
        // Reestablish the pre-failure baseline immediately. Only the additional
        // recovery boost is ramped; otherwise reconnect could strand cooling at 0%.
        let power = if self.target > previous_target {
            0.0
        } else {
            limit_power(
                remembered_power,
                demand,
                self.power_tick.elapsed().as_secs_f64(),
                self.power_limit(),
            )
        };
        self.write_power(camera, power)?;
        self.power_tick = Instant::now();
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
        self.restore_actuators(camera, true)
    }
    /// Sensor initialization must preserve the regulation deadline: repeatedly
    /// taking short frames must not postpone the one-second feedback service.
    pub fn reapply(&mut self, camera: &Camera) -> Result<()> {
        self.restore_actuators(camera, false)
    }
    fn restore_actuators(&mut self, camera: &Camera, reset_tick: bool) -> Result<()> {
        let observed = Instant::now();
        self.write_power(camera, if self.enabled { self.power } else { 0.0 })?;
        flags(camera, 0x80, !self.enabled)?;
        if self.heater {
            // Traced SDK heater output preset: BD2a=0xc5 (197) on, zero off,
            // plus BD19 bit 0x40. This is not a wattage or percentage setting.
            camera.vendor(0xbd, 0x2a, if self.dew { 197 } else { 0 }, 0)?;
            flags(camera, 0x40, self.dew)?;
        }
        if let Some((fan, led)) = self.auxiliary {
            self.set(camera, 22, fan)?;
            self.set(camera, 23, led)?;
        }
        self.verify_actuators(camera)?;
        self.finish_restore(Self::read_temperature(camera)?, observed, reset_tick);
        Ok(())
    }
    pub(super) fn verify_actuators(&self, camera: &Camera) -> Result<()> {
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
        Ok(())
    }
    fn finish_restore(&mut self, temperature: f64, observed: Instant, reset_tick: bool) {
        self.temperature = temperature;
        for index in [0, 1, 3] {
            self.observed_at[index] = observed;
        }
        if reset_tick {
            self.tick = Instant::now();
            self.power_tick = self.tick;
        }
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
            power_tick: Instant::now(),
            live_power_limit: None,
            max_power_change: regain_core::RecoveryOptions::default()
                .cooler_max_power_change_per_5_seconds,
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
        // ASI2600/6200: FPGA register BD26; ASI585: immediate DAC command B2,
        // with its code in USB wValue (no register address and no IN readback).
        match self.output {
            CoolerOutput::Fpga => camera.vendor(0xbd, 0x26, power_register(percent), 0)?,
            CoolerOutput::Dac => camera.vendor(0xb2, power_dac(percent), 0, 0)?,
        };
        Ok(())
    }
    fn read_temperature(camera: &Camera) -> Result<f64> {
        // B3 returns signed little-endian Q8.8 degrees C, not tenths of a degree.
        // The host protocol separately converts this to control 8's tenths.
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
                    let power = self.limited_power(power, observed);
                    self.write_power(camera, power)?;
                    self.power_tick = Instant::now();
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
                    // Starting from disabled must not spend idle time as ramp
                    // credit. The first feedback tick begins the controlled rise.
                    if value != 0 {
                        self.power_tick = observed;
                    }
                    let power = if value == 0 {
                        0.0
                    } else {
                        self.temperature = self.feedback(camera)?;
                        self.observed_at[0] = observed;
                        (PROPORTIONAL * (self.temperature - self.target as f64)).clamp(0.0, 100.0)
                    };
                    let power = if value == 0 {
                        0.0
                    } else {
                        self.limited_power(power, observed)
                    };
                    self.write_power(camera, power)?;
                    self.power_tick = Instant::now();
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
                // Both writes are required by the SDK mapping; changing the
                // flag alone does not establish the traced heater output.
                camera.vendor(0xbd, 0x2a, if value == 0 { 0 } else { 197 }, 0)?;
                flags(camera, 0x40, value != 0)?;
                self.dew = value != 0;
            }
            22 | 23 if self.auxiliary.is_some() => {
                // Capability-gated P25 auxiliaries: BDfa fan command, BDfb LED
                // brightness, raw 0..255. Readback confirms the command, not RPM.
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
    fn service_interval(&mut self, observed: Instant) -> Option<f64> {
        let elapsed = observed.duration_since(self.tick).as_secs_f64();
        if elapsed < 1.0 {
            return None;
        }
        self.tick = observed;
        Some(elapsed)
    }
    pub fn service(&mut self, camera: &Camera) -> Result<()> {
        let observed = Instant::now();
        let Some(elapsed) = self.service_interval(observed) else {
            return Ok(());
        };
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
            let maximum = self.power_limit();
            let power = regulate(self.power, &mut self.integral, error, elapsed, maximum);
            let power = self.limited_power(power, observed);
            self.write_power(camera, power)?;
            self.power = power;
            self.power_tick = Instant::now();
            self.observed_at[1] = observed;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configurable_slew_bounds_five_seconds_in_both_directions_and_after_stalls() {
        for maximum in [1.0, 25.0, 100.0] {
            for seconds in [0.01, 0.2, 1.0] {
                let mut rising = 0.0;
                let mut falling = 100.0;
                for _ in 0..(5.0 / seconds) as usize {
                    rising = limit_power(rising, 100.0, seconds, maximum);
                    falling = limit_power(falling, 0.0, seconds, maximum);
                }
                assert!((rising - maximum).abs() < 1e-8);
                assert!((falling - (100.0 - maximum)).abs() < 1e-8);
            }
            assert_eq!(limit_power(0.0, 100.0, 600.0, maximum), maximum * 2.0 / 5.0);
            assert_eq!(limit_power(40.0, 100.0, 0.0, maximum), 40.0);
        }
        // Setpoint responses pass through the same limiter as PI feedback.
        let (demand, _) = target_change(40.0, 40.0, -10.0, -10.0, -30.0);
        assert_eq!(limit_power(40.0, demand, 1.0, 25.0), 45.0);
        let (demand, _) = target_change(96.0, 40.0, -8.0, -15.0, -5.0);
        assert_eq!(limit_power(96.0, demand, 1.0, 25.0), 91.0);
    }
    #[test]
    fn short_frame_reapplication_keeps_feedback_cadence_and_regulator_history() {
        use std::time::Duration;
        let origin = Instant::now() - Duration::from_secs(60);
        let mut environment = Environment {
            target: 5,
            enabled: true,
            dew: true,
            temperature: 10.0,
            power: 10.0,
            integral: 10.0,
            tick: origin,
            power_tick: origin,
            max_power_change: 25.0,
            live_power_limit: None,
            auxiliary: None,
            heater: true,
            output: CoolerOutput::Fpga,
            observed_at: [origin; 4],
        };
        let mut feedbacks = 0;
        // Ten successful initialization/reapply completions at 5 fps must still
        // allow two regulator updates. Resetting tick on every frame starves it.
        for millis in (200..=2000).step_by(200) {
            let observed = origin + Duration::from_millis(millis);
            let prior = (
                environment.target,
                environment.power,
                environment.integral,
                environment.tick,
            );
            environment.finish_restore(10.0, observed, false);
            assert_eq!(
                (
                    environment.target,
                    environment.power,
                    environment.integral,
                    environment.tick
                ),
                prior
            );
            if let Some(elapsed) = environment.service_interval(observed) {
                assert_eq!(elapsed, 1.0);
                feedbacks += 1;
                environment.power = regulate(
                    environment.power,
                    &mut environment.integral,
                    environment.temperature - environment.target as f64,
                    elapsed,
                    environment.max_power_change,
                );
            }
        }
        assert_eq!(feedbacks, 2);
        assert!(environment.power > 10.0);
        let reconnect = Instant::now();
        environment.finish_restore(10.0, reconnect, true);
        assert!(environment.tick >= reconnect);
        let tick = environment.tick;
        assert_eq!(
            environment.service_interval(tick + Duration::from_millis(999)),
            None
        );
        assert_eq!(
            environment.service_interval(tick + Duration::from_secs(1)),
            Some(1.0)
        );
    }

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
        assert!(regulate(power, &mut integral, 2.0, 1.0, 25.0) >= power);
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
            let mut power = 0.0;
            let mut integral = 0.0;
            let mut actuator = 0.0;
            let mut previous = 0.0;
            for target in [0.0, -5.0, 5.0] {
                if target != previous {
                    let prior_power = power;
                    let (demand, next_integral) =
                        target_change(power, integral, temperature, previous, target);
                    if target < previous {
                        assert!(demand > prior_power + 20.0);
                    } else {
                        assert_eq!(demand, 0.0);
                    }
                    // Match the real setpoint path: demand also obeys the slew limit.
                    power = limit_power(power, demand, 1.0, 25.0);
                    integral = next_integral;
                }
                for second in 0..1200 {
                    power = regulate(power, &mut integral, temperature - target, 1.0, 25.0);
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
            power = regulate(power, &mut integral, 100.0, 1.0, 25.0);
        }
        assert_eq!(power, 100.0);
        assert_eq!(integral, 0.0);
        assert_eq!(regulate(power, &mut integral, 0.0, 1.0, 25.0), 95.0);
        assert_eq!(regulate(0.0, &mut integral, 100.0, 600.0, 25.0), 10.0);
        assert_eq!(regulate(0.0, &mut integral, -10.0, 1.0, 25.0), 0.0);
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
                power = regulate(power, &mut integral, temperature - target, 1.0, 25.0);
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
            power_tick: Instant::now(),
            max_power_change: 25.0,
            live_power_limit: None,
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
