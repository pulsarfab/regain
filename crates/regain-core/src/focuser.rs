//! Opt-in continuous focuser compensation. One controller owns both move legs;
//! vendor transports never implement their own temperature compensation loop.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Backlash {
    #[default]
    Hardware,
    Regain,
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Approach {
    #[default]
    Increasing,
    Decreasing,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    pub continuous: bool,
    pub steps_per_celsius: f64,
    pub deadband_steps: u32,
    pub interval_seconds: f64,
    pub max_correction_steps: u32,
    pub backlash: Backlash,
    pub backlash_steps: u32,
    pub approach: Approach,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            continuous: false,
            steps_per_celsius: 0.0,
            deadband_steps: 5,
            interval_seconds: 30.0,
            max_correction_steps: 1000,
            backlash: Backlash::Hardware,
            backlash_steps: 0,
            approach: Approach::Increasing,
        }
    }
}
impl Options {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.steps_per_celsius.is_finite() && self.steps_per_celsius.abs() <= 100_000.0,
            "Steps per degree must be finite and between -100000 and 100000"
        );
        ensure!(
            (1..=10_000).contains(&self.deadband_steps),
            "Deadband must be 1..10000 steps"
        );
        ensure!(
            self.interval_seconds.is_finite() && (1.0..=3600.0).contains(&self.interval_seconds),
            "Sample interval must be 1..3600 seconds"
        );
        ensure!(
            (1..=100_000).contains(&self.max_correction_steps),
            "Maximum correction must be 1..100000 steps"
        );
        ensure!(
            self.backlash_steps <= 100_000,
            "Backlash must be 0..100000 steps"
        );
        Ok(())
    }
}
/// The same keys, labels, defaults and bounds drive native and web setup.
pub fn schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{
        "continuous":{"type":"boolean","default":false,"title":"Allow continuous compensation","description":"Opt in to automatic moves, including during exposures. Enable TempComp explicitly after connecting; it is never restored on reconnect."},
        "stepsPerCelsius":{"type":"number","default":0,"minimum":-100000,"maximum":100000,"title":"Steps per degree Celsius","description":"Signed slope calibrated for this telescope and focuser. Zero must be replaced before enabling compensation."},
        "deadbandSteps":{"type":"integer","default":5,"minimum":1,"maximum":10000,"title":"Minimum correction (steps)","description":"Accumulate temperature changes until at least this many steps are required."},
        "intervalSeconds":{"type":"number","default":30,"minimum":1,"maximum":3600,"title":"Temperature sample interval (seconds)","description":"The median of the last three samples rejects isolated temperature noise."},
        "maxCorrectionSteps":{"type":"integer","default":1000,"minimum":1,"maximum":100000,"title":"Maximum automatic correction (steps)","description":"Larger corrections suspend tracking instead of making an unexpected large move."},
        "backlash":{"type":"string","enum":["hardware","regain"],"default":"hardware","title":"Backlash owner","description":"Hardware uses the device setting. Regain approaches every move from one direction and requires zero device backlash. Disable NINA backlash when either owns it."},
        "backlashSteps":{"type":"integer","default":0,"minimum":0,"maximum":100000,"title":"Regain overshoot (steps)","description":"Used only with Regain backlash. Calibrate focus with this same approach before enabling tracking."},
        "approach":{"type":"string","enum":["increasing","decreasing"],"default":"increasing","title":"Regain final approach","description":"Final direction of every explicit and automatic move. Both legs must fit within travel limits."}
    }})
}

pub trait Device {
    fn status(&mut self) -> Result<Value>;
    fn move_to(&mut self, position: i32) -> Result<()>;
    fn halt(&mut self) -> Result<()>;
    fn extra(&mut self, request: Value) -> Result<Value>;
}
struct Sample {
    position: i32,
    moving: bool,
    temperature: Option<f64>,
    maximum: i32,
    backlash: u64,
}
fn sample(value: &Value) -> Result<Sample> {
    ensure!(
        value["error"] == 0 && value["fault"].is_null(),
        "Focuser hardware fault: {value}"
    );
    let position = i32::try_from(
        value["position"]
            .as_i64()
            .context("Invalid focuser position")?,
    )?;
    let maximum = i32::try_from(
        value["max_step"]
            .as_i64()
            .context("Invalid focuser limit")?,
    )?;
    ensure!(
        maximum >= 0 && (0..=maximum).contains(&position),
        "Focuser position outside travel limits"
    );
    Ok(Sample {
        position,
        maximum,
        moving: value["moving"]
            .as_bool()
            .context("Invalid movement state")?,
        temperature: value["temperature_c"]
            .as_f64()
            .filter(|t| t.is_finite() && (-55.0..=125.0).contains(t)),
        backlash: value["backlash"]
            .as_u64()
            .context("Invalid hardware backlash")?,
    })
}
struct Motion {
    target: i32,
    next: Option<i32>,
    automatic: bool,
    started: Duration,
}
pub struct Controller<D> {
    pub device: D,
    options: Options,
    enabled: bool,
    reference: Option<(i32, f64)>,
    temperatures: VecDeque<f64>,
    motion: Option<Motion>,
    expected: Option<i32>,
    last_sample: Duration,
    last_poll: Duration,
    error: Option<String>,
}
impl<D: Device> Controller<D> {
    pub fn new(device: D) -> Self {
        Self {
            device,
            options: Options::default(),
            enabled: false,
            reference: None,
            temperatures: VecDeque::new(),
            motion: None,
            expected: None,
            last_sample: Duration::ZERO,
            last_poll: Duration::ZERO,
            error: None,
        }
    }
    fn suspend(&mut self, reason: String) {
        self.enabled = false;
        self.error = Some(reason);
        if self.motion.take().is_some()
            && let Err(error) = self.device.halt()
        {
            self.error = Some(format!(
                "{}; halt failed: {error:#}",
                self.error.as_deref().unwrap()
            ));
        }
    }
    fn plan(
        &self,
        position: i32,
        maximum: i32,
        hardware_backlash: u64,
    ) -> Result<(i32, Option<i32>)> {
        ensure!(
            (0..=maximum).contains(&position),
            "Requested position is outside travel limits"
        );
        if self.options.backlash == Backlash::Regain {
            ensure!(
                hardware_backlash == 0,
                "Set device backlash to zero before using Regain backlash"
            );
            let offset = i64::from(self.options.backlash_steps)
                * if self.options.approach == Approach::Increasing {
                    -1
                } else {
                    1
                };
            let first = i64::from(position) + offset;
            ensure!(
                (0..=i64::from(maximum)).contains(&first),
                "Backlash approach is outside travel limits"
            );
            if first != i64::from(position) {
                return Ok((first as i32, Some(position)));
            }
        }
        Ok((position, None))
    }
    fn start(&mut self, position: i32, s: &Sample, automatic: bool, now: Duration) -> Result<()> {
        let (first, next) = self.plan(position, s.maximum, s.backlash)?;
        // Record ownership before dispatch: a lost acknowledgement is uncertain,
        // must be halted and must never be replayed automatically.
        self.motion = Some(Motion {
            target: first,
            next,
            automatic,
            started: now,
        });
        if let Err(error) = self.device.move_to(first) {
            self.suspend(format!("Move acknowledgement failed: {error:#}"));
            return Err(error);
        }
        Ok(())
    }
    pub fn request(&mut self, request: Value, now: Duration) -> Result<Value> {
        match request["command"].as_str().unwrap_or("") {
            // Read-only preflight gives frontends a typed invalid-position
            // response without guessing from hardware/transport error strings.
            // start() repeats this check immediately before actual dispatch.
            "validate-move" => {
                let position = i32::try_from(
                    request["position"]
                        .as_i64()
                        .context("Expected integer position")?,
                )?;
                let s = sample(&self.device.status()?)?;
                match self.plan(position, s.maximum, s.backlash) {
                    Ok(_) => Ok(json!({"valid":true})),
                    Err(error) => Ok(json!({"valid":false,"reason":error.to_string()})),
                }
            }
            "status" => {
                let mut value = self.device.status()?;
                let available = match sample(&value) {
                    Ok(s) => {
                        if self.enabled && s.temperature.is_none() {
                            self.suspend("Temperature sensor is unavailable".into());
                            value = self.device.status()?;
                        }
                        value["temperature_c"] = json!(s.temperature);
                        s.temperature.is_some()
                    }
                    Err(error) => {
                        if self.enabled || self.motion.is_some() {
                            self.suspend(format!("Focuser fault: {error:#}"));
                        }
                        false
                    }
                };
                value["temp_comp_available"] = json!(
                    available && self.options.continuous && self.options.steps_per_celsius != 0.0
                );
                value["temp_comp"] = json!(self.enabled);
                value["temperature_compensation"] = json!({"options":self.options,"enabled":self.enabled,
                    "referencePosition":self.reference.map(|r|r.0),"referenceTemperature":self.reference.map(|r|r.1),
                    "target":self.motion.as_ref().map(|m|m.next.unwrap_or(m.target)),"lastError":self.error});
                if self.motion.is_some() {
                    value["moving"] = json!(true);
                }
                Ok(value)
            }
            "temperature-compensation" => {
                ensure!(
                    request.as_object().is_some_and(|o| o
                        .keys()
                        .all(|k| ["command", "options", "enabled"].contains(&k.as_str()))),
                    "Unknown temperature compensation parameter"
                );
                if request.get("options").is_none() && request["enabled"] == false {
                    self.enabled = false;
                    return Ok(Value::Null);
                }
                if request.get("options").is_none() && request["enabled"] == true && self.enabled {
                    return Ok(Value::Null);
                }
                let s = sample(&self.device.status()?)?;
                ensure!(
                    !s.moving && self.motion.is_none(),
                    "Wait for the focuser to stop before changing compensation"
                );
                let next = request
                    .get("options")
                    .map(|v| serde_json::from_value::<Options>(v.clone()))
                    .transpose()?
                    .unwrap_or_else(|| self.options.clone());
                next.validate()?;
                let enabled = request
                    .get("enabled")
                    .map(|v| v.as_bool().context("Expected boolean enabled"))
                    .transpose()?
                    .unwrap_or(false);
                if enabled {
                    ensure!(
                        next.continuous && next.steps_per_celsius != 0.0,
                        "Configure continuous mode and a nonzero steps-per-degree coefficient first"
                    );
                    ensure!(s.temperature.is_some(), "Temperature sensor is unavailable");
                    if next.backlash == Backlash::Regain {
                        ensure!(
                            s.backlash == 0,
                            "Set device backlash to zero before using Regain backlash"
                        );
                    }
                }
                self.options = next;
                self.enabled = enabled;
                self.error = None;
                self.reference = s.temperature.map(|t| (s.position, t));
                self.expected = Some(s.position);
                self.temperatures.clear();
                if let Some(t) = s.temperature {
                    self.temperatures.push_back(t);
                }
                self.last_sample = now;
                Ok(Value::Null)
            }
            "move" => {
                let position = i32::try_from(
                    request["position"]
                        .as_i64()
                        .context("Expected integer position")?,
                )?;
                let mut s = sample(&self.device.status()?)?;
                self.plan(position, s.maximum, s.backlash)?;
                // IFocuserV3+: TempComp=true must not itself prevent Move.
                if self.motion.as_ref().is_some_and(|m| m.automatic) {
                    if let Err(error) = self.device.halt() {
                        self.motion = None;
                        self.enabled = false;
                        self.error = Some(format!(
                            "Cannot interrupt compensation; movement is uncertain: {error:#}"
                        ));
                        return Err(error);
                    }
                    self.motion = None;
                    s = sample(&self.device.status()?)?;
                }
                ensure!(
                    !s.moving && self.motion.is_none(),
                    "Focuser is already moving"
                );
                self.start(position, &s, false, now)?;
                Ok(Value::Null)
            }
            "halt" => {
                self.enabled = false;
                self.motion = None;
                if let Err(error) = self.device.halt() {
                    self.error = Some(format!(
                        "Halt failed; movement state is uncertain: {error:#}"
                    ));
                    return Err(error);
                }
                Ok(Value::Null)
            }
            "settings" => {
                ensure!(
                    !self.enabled && self.motion.is_none(),
                    "Disable temperature compensation and wait for motion before changing motor settings"
                );
                self.device.extra(request)
            }
            _ => self.device.extra(request),
        }
    }
    pub fn poll(&mut self, now: Duration) {
        if now.saturating_sub(self.last_poll) < Duration::from_millis(250)
            || (!self.enabled && self.motion.is_none())
        {
            return;
        }
        self.last_poll = now;
        if let Err(error) = self.advance(now) {
            self.suspend(format!("Temperature compensation suspended: {error:#}"));
        }
    }
    fn advance(&mut self, now: Duration) -> Result<()> {
        let s = sample(&self.device.status()?)?;
        if self.enabled {
            ensure!(s.temperature.is_some(), "Temperature sensor is unavailable");
        }
        if let Some(m) = &mut self.motion {
            ensure!(
                now.saturating_sub(m.started) < Duration::from_secs(600),
                "Focuser movement timed out"
            );
            if s.moving {
                return Ok(());
            }
            if s.position != m.target {
                ensure!(
                    now.saturating_sub(m.started) < Duration::from_secs(2),
                    "Focuser stopped short of requested position"
                );
                return Ok(());
            }
            if let Some(next) = m.next.take() {
                m.target = next;
                m.started = now;
                self.device.move_to(next)?;
                return Ok(());
            }
            let manual = !m.automatic;
            self.motion = None;
            self.expected = Some(s.position);
            if manual && self.enabled {
                let t = s.temperature.context("Temperature sensor is unavailable")?;
                self.reference = Some((s.position, t));
                self.temperatures.clear();
                self.temperatures.push_back(t);
                self.last_sample = now;
            }
            return Ok(());
        }
        if !self.enabled {
            return Ok(());
        }
        ensure!(
            !s.moving && self.expected == Some(s.position),
            "Focuser moved outside the compensation controller; re-enable to establish a new reference"
        );
        if now.saturating_sub(self.last_sample).as_secs_f64() < self.options.interval_seconds {
            return Ok(());
        }
        self.last_sample = now;
        self.temperatures.push_back(s.temperature.unwrap());
        if self.temperatures.len() > 3 {
            self.temperatures.pop_front();
        }
        if self.temperatures.len() < 3 {
            return Ok(());
        }
        let mut values: Vec<_> = self.temperatures.iter().copied().collect();
        values.sort_by(f64::total_cmp);
        let temperature = values[values.len() / 2];
        let (position, reference) = self
            .reference
            .context("Temperature reference is unavailable")?;
        let target = (f64::from(position)
            + self.options.steps_per_celsius * (temperature - reference))
            .round();
        ensure!(
            target.is_finite() && (0.0..=f64::from(s.maximum)).contains(&target),
            "Temperature target is outside travel limits"
        );
        let target = target as i32;
        let delta = s.position.abs_diff(target);
        if delta < self.options.deadband_steps {
            return Ok(());
        }
        ensure!(
            delta <= self.options.max_correction_steps,
            "Temperature correction exceeds the configured maximum"
        );
        self.start(target, &s, true, now)
    }
    pub fn shutdown(&mut self) -> Result<()> {
        self.enabled = false;
        if self.motion.take().is_some() {
            self.device.halt()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        position: i32,
        temperature: Option<f64>,
        moving: bool,
        backlash: u32,
        moves: Vec<i32>,
        halts: usize,
        fail_move: bool,
        fail_halt: bool,
        fault: Option<&'static str>,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                position: 500,
                temperature: Some(20.0),
                moving: false,
                backlash: 0,
                moves: vec![],
                halts: 0,
                fail_move: false,
                fail_halt: false,
                fault: None,
            }
        }
    }
    impl Device for Fake {
        fn status(&mut self) -> Result<Value> {
            Ok(
                json!({"position":self.position,"moving":self.moving,"temperature_c":self.temperature,"backlash":self.backlash,"max_step":1000,"error":0,"fault":self.fault}),
            )
        }
        fn move_to(&mut self, position: i32) -> Result<()> {
            self.moves.push(position);
            self.moving = true;
            ensure!(!self.fail_move, "Lost acknowledgement");
            Ok(())
        }
        fn halt(&mut self) -> Result<()> {
            self.halts += 1;
            ensure!(!self.fail_halt, "Halt acknowledgement lost");
            self.moving = false;
            Ok(())
        }
        fn extra(&mut self, _: Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }
    fn at(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }
    fn request(c: &mut Controller<Fake>, v: Value, seconds: u64) -> Value {
        c.request(v, at(seconds)).unwrap()
    }
    fn status(c: &mut Controller<Fake>) -> Value {
        request(c, json!({"command":"status"}), 0)
    }
    fn configured(options: Value) -> Controller<Fake> {
        let mut c = Controller::new(Fake::default());
        request(
            &mut c,
            json!({"command":"temperature-compensation","options":options}),
            0,
        );
        c
    }
    fn enabled(options: Value) -> Controller<Fake> {
        let mut c = configured(options);
        request(
            &mut c,
            json!({"command":"temperature-compensation","enabled":true}),
            0,
        );
        c
    }
    fn base() -> Value {
        json!({"continuous":true,"stepsPerCelsius":100,"intervalSeconds":1})
    }
    fn arrive(c: &mut Controller<Fake>, seconds: u64) {
        c.device.position = *c.device.moves.last().unwrap();
        c.device.moving = false;
        c.poll(at(seconds));
    }
    #[test]
    fn configuration_is_strict_opt_in_and_never_restores_enabled() {
        let mut c = Controller::new(Fake::default());
        assert_eq!(status(&mut c)["temp_comp"], false);
        assert_eq!(status(&mut c)["temp_comp_available"], false);
        assert!(
            c.request(
                json!({"command":"temperature-compensation","enabled":true}),
                at(0)
            )
            .is_err()
        );
        for bad in [
            json!({"typo":1}),
            json!({"continuous":"true"}),
            json!({"intervalSeconds":0}),
            json!({"deadbandSteps":1.5}),
            json!({"backlash":"nina"}),
            json!({"stepsPerCelsius":100001}),
        ] {
            assert!(
                c.request(
                    json!({"command":"temperature-compensation","options":bad}),
                    at(0)
                )
                .is_err()
            );
        }
        request(
            &mut c,
            json!({"command":"temperature-compensation","options":base(),"enabled":true}),
            0,
        );
        assert_eq!(status(&mut c)["temp_comp"], true);
        assert_eq!(status(&mut c)["temp_comp_available"], true);
        request(
            &mut c,
            json!({"command":"temperature-compensation","options":base()}),
            0,
        );
        assert_eq!(status(&mut c)["temp_comp"], false);
        for (key, value) in serde_json::to_value(Options::default())
            .unwrap()
            .as_object()
            .unwrap()
        {
            let default = &schema()["properties"][key]["default"];
            if value.is_number() {
                assert_eq!(default.as_f64(), value.as_f64());
            } else {
                assert_eq!(default, value);
            }
        }
        assert_eq!(
            serde_json::to_value(Options::default())
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            schema()["properties"].as_object().unwrap().len()
        );
    }
    #[test]
    fn median_deadband_and_signed_fixed_reference_prevent_noise_and_drift() {
        for slope in [-100, 100] {
            let mut options = base();
            options["stepsPerCelsius"] = json!(slope);
            let mut c = enabled(options);
            c.device.temperature = Some(24.0);
            c.poll(at(1)); // isolated spike
            c.device.temperature = Some(20.0);
            c.poll(at(2));
            assert!(c.device.moves.is_empty());
            c.device.temperature = Some(20.04);
            c.poll(at(3));
            c.poll(at(4));
            assert!(c.device.moves.is_empty()); // four steps, below five-step deadband
            c.device.temperature = Some(20.2);
            c.poll(at(5));
            c.poll(at(6));
            assert_eq!(c.device.moves, vec![500 + slope / 5]);
            arrive(&mut c, 7);
            c.device.temperature = Some(20.3);
            c.poll(at(8));
            c.poll(at(9));
            assert_eq!(c.device.moves.last(), Some(&(500 + 3 * slope / 10))); // original reference, not accumulated rounds
        }
    }
    #[test]
    fn move_with_tempcomp_enabled_finishes_both_legs_and_rebases() {
        let mut options = base();
        options["backlash"] = json!("regain");
        options["backlashSteps"] = json!(20);
        let mut c = enabled(options);
        request(&mut c, json!({"command":"move","position":600}), 0);
        assert_eq!(c.device.moves, vec![580]);
        assert_eq!(status(&mut c)["moving"], true);
        arrive(&mut c, 1);
        assert_eq!(c.device.moves, vec![580, 600]);
        assert_eq!(status(&mut c)["moving"], true);
        c.device.temperature = Some(21.0);
        arrive(&mut c, 2);
        let state = status(&mut c);
        assert_eq!(state["moving"], false);
        assert_eq!(state["temp_comp"], true);
        assert_eq!(state["temperature_compensation"]["referencePosition"], 600);
        assert_eq!(
            state["temperature_compensation"]["referenceTemperature"],
            21.0
        );
        c.poll(at(3));
        c.poll(at(4));
        assert_eq!(c.device.moves, vec![580, 600]);
    }
    #[test]
    fn explicit_move_interrupts_automatic_move_but_invalid_move_does_not() {
        let mut c = enabled(base());
        c.device.temperature = Some(21.0);
        c.poll(at(1));
        c.poll(at(2));
        assert_eq!(c.device.moves, vec![600]);
        assert!(
            c.request(json!({"command":"move","position":1001}), at(2))
                .is_err()
        );
        assert_eq!(c.device.halts, 0);
        request(&mut c, json!({"command":"move","position":700}), 2);
        assert_eq!(c.device.halts, 1);
        assert_eq!(c.device.moves, vec![600, 700]);
        arrive(&mut c, 3);
        assert_eq!(
            status(&mut c)["temperature_compensation"]["referencePosition"],
            700
        );
    }
    #[test]
    fn backlash_has_one_owner_and_both_legs_are_checked_before_dispatch() {
        for (direction, target, first) in [("increasing", 500, 480), ("decreasing", 500, 520)] {
            let mut c =
                configured(json!({"backlash":"regain","backlashSteps":20,"approach":direction}));
            c.device.backlash = 1;
            assert!(
                c.request(json!({"command":"move","position":target}), at(0))
                    .is_err()
            );
            c.device.backlash = 0;
            let invalid = if direction == "increasing" { 0 } else { 1000 };
            assert!(
                c.request(json!({"command":"move","position":invalid}), at(0))
                    .is_err()
            );
            assert!(c.device.moves.is_empty());
            request(&mut c, json!({"command":"move","position":target}), 0);
            assert_eq!(c.device.moves, vec![first]);
        }
        let mut c = enabled(base());
        c.device.backlash = 20;
        request(&mut c, json!({"command":"move","position":600}), 0);
        assert_eq!(c.device.moves, vec![600]); // firmware owns backlash
    }
    #[test]
    fn disabling_finishes_current_backlash_but_halt_cancels_all_legs_and_tracking() {
        for halt in [false, true] {
            let mut c = enabled(
                json!({"continuous":true,"stepsPerCelsius":100,"backlash":"regain","backlashSteps":20}),
            );
            request(&mut c, json!({"command":"move","position":600}), 0);
            request(
                &mut c,
                if halt {
                    json!({"command":"halt"})
                } else {
                    json!({"command":"temperature-compensation","enabled":false})
                },
                0,
            );
            assert_eq!(status(&mut c)["temp_comp"], false);
            c.device.position = 580;
            c.device.moving = false;
            c.poll(at(1));
            assert_eq!(c.device.moves.len(), if halt { 1 } else { 2 });
            c.poll(at(100));
            assert_eq!(status(&mut c)["temp_comp"], false);
        }
    }
    #[test]
    fn sensor_loss_external_motion_limits_and_large_changes_suspend_without_replay() {
        for fault in 0..7 {
            let mut c = enabled(base());
            match fault {
                0 => c.device.temperature = None,
                1 => c.device.position += 1,
                2 => c.device.temperature = Some(30.0), // outside travel
                3 => {
                    c.options.max_correction_steps = 10;
                    c.device.temperature = Some(21.0);
                }
                4 => c.device.moving = true,
                5 => c.device.fault = Some("Motor fault"),
                _ => c.device.temperature = Some(900.0),
            }
            c.poll(at(1));
            c.poll(at(2));
            assert_eq!(status(&mut c)["temp_comp"], false);
            if fault == 6 {
                assert!(status(&mut c)["temperature_c"].is_null());
            }
            assert!(c.device.moves.is_empty());
            assert!(c.error.is_some());
            c.device.temperature = Some(20.0);
            c.device.moving = false;
            c.poll(at(100));
            assert!(c.device.moves.is_empty());
        }
    }
    #[test]
    fn status_keeps_fault_diagnostics_and_lost_halt_is_not_repeated() {
        let mut c = enabled(base());
        c.device.temperature = Some(21.0);
        c.poll(at(1));
        c.poll(at(2));
        c.device.fail_halt = true;
        assert!(
            c.request(json!({"command":"move","position":700}), at(2))
                .is_err()
        );
        assert_eq!(c.device.halts, 1);
        c.poll(at(100));
        assert_eq!(c.device.halts, 1);
        assert_eq!(status(&mut c)["temp_comp"], false);
        assert!(
            status(&mut c)["temperature_compensation"]["lastError"]
                .as_str()
                .unwrap()
                .contains("uncertain")
        );
        let mut c = enabled(base());
        request(&mut c, json!({"command":"move","position":600}), 0);
        c.device.fault = Some("Motor fault");
        let diagnostic = status(&mut c);
        assert_eq!(diagnostic["fault"], "Motor fault");
        assert_eq!(diagnostic["temp_comp"], false);
        assert!(!diagnostic["temperature_compensation"]["lastError"].is_null());
        assert_eq!(c.device.halts, 1);
    }
    #[test]
    fn uncertain_acknowledgements_and_timeouts_stop_tracking_and_never_retry() {
        for second_leg in [false, true] {
            let mut c = enabled(
                json!({"continuous":true,"stepsPerCelsius":100,"backlash":"regain","backlashSteps":20}),
            );
            if second_leg {
                request(&mut c, json!({"command":"move","position":600}), 0);
                c.device.fail_move = true;
                arrive(&mut c, 1);
            } else {
                c.device.fail_move = true;
                assert!(
                    c.request(json!({"command":"move","position":600}), at(0))
                        .is_err()
                );
            }
            let count = c.device.moves.len();
            c.poll(at(1000));
            assert_eq!(c.device.moves.len(), count);
            assert_eq!(c.device.halts, 1);
            assert_eq!(status(&mut c)["temp_comp"], false);
        }
        let mut c = enabled(base());
        request(&mut c, json!({"command":"move","position":600}), 0);
        c.poll(at(600));
        assert_eq!(c.device.halts, 1);
        assert_eq!(status(&mut c)["temp_comp"], false);
        let mut c = enabled(base());
        c.device.fail_halt = true;
        assert!(c.request(json!({"command":"halt"}), at(0)).is_err());
        assert!(c.error.unwrap().contains("uncertain"));
    }
}
