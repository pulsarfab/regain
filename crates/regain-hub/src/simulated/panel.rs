use super::{Fault, invalid, unsupported};
use crate::source::{ErrorKind, SourceError, Values};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverCalibratorState {
    pub brightness: i32,
    pub max_brightness: i32,
    pub cover_state: i32,
    pub calibrator_state: i32,
    pub cover_moving: bool,
    pub calibrator_changing: bool,
    pub move_duration_seconds: f64,
    pub light_duration_seconds: f64,
}
impl Default for CoverCalibratorState {
    fn default() -> Self {
        Self {
            brightness: 0,
            max_brightness: 4096,
            cover_state: 1,
            calibrator_state: 1,
            cover_moving: false,
            calibrator_changing: false,
            move_duration_seconds: 0.2,
            light_duration_seconds: 0.2,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverCalibratorUpdate {
    #[schemars(range(min = 0, max = 2147483647))]
    pub brightness: Option<i32>,
    #[schemars(range(min = 1, max = 2147483647))]
    pub max_brightness: Option<i32>,
    #[schemars(range(min = 0, max = 5))]
    pub cover_state: Option<i32>,
    #[schemars(range(min = 0, max = 5))]
    pub calibrator_state: Option<i32>,
    pub cover_moving: Option<bool>,
    pub calibrator_changing: Option<bool>,
    #[schemars(range(min = 0, max = 300))]
    pub move_duration_seconds: Option<f64>,
    #[schemars(range(min = 0, max = 300))]
    pub light_duration_seconds: Option<f64>,
}
impl CoverCalibratorUpdate {
    pub(super) fn replaces_cover(&self) -> bool {
        self.cover_state.is_some() || self.cover_moving.is_some()
    }
    pub(super) fn replaces_light(&self) -> bool {
        self.brightness.is_some()
            || self.max_brightness.is_some()
            || self.calibrator_state.is_some()
            || self.calibrator_changing.is_some()
    }
    pub(super) fn apply(self, state: &mut CoverCalibratorState) -> Result<(), SourceError> {
        macro_rules! apply { ($($key:ident),*) => { $(if let Some(value) = self.$key { state.$key = value; })* }; }
        apply!(
            brightness,
            max_brightness,
            cover_state,
            calibrator_state,
            cover_moving,
            calibrator_changing,
            move_duration_seconds,
            light_duration_seconds
        );
        if state.max_brightness <= 0
            || !(0..=state.max_brightness).contains(&state.brightness)
            || !(0..=5).contains(&state.cover_state)
            || !(0..=5).contains(&state.calibrator_state)
            || state.cover_state == 0 && state.cover_moving
            || state.calibrator_state <= 1 && state.brightness != 0
            || state.calibrator_state == 0 && state.calibrator_changing
            || !state.move_duration_seconds.is_finite()
            || !(0.0..=300.0).contains(&state.move_duration_seconds)
            || !state.light_duration_seconds.is_finite()
            || !(0.0..=300.0).contains(&state.light_duration_seconds)
        {
            return Err(invalid("Invalid simulated cover/calibrator state"));
        }
        Ok(())
    }
}

/// Cover motion and light readiness share one actor but independent clocks.
#[derive(Default)]
pub(super) struct PanelMotion {
    cover: Option<(Instant, i32)>,
    light: Option<(Instant, i32)>,
}
impl PanelMotion {
    pub(super) fn replace(&mut self, cover: bool, light: bool) {
        if cover {
            self.cover = None;
        }
        if light {
            self.light = None;
        }
    }
    pub(super) fn effective(&self, state: &mut CoverCalibratorState, fault: Fault) {
        if fault == Fault::StalledMotion {
            return;
        }
        if let Some((at, target)) = self.cover
            && at <= Instant::now()
        {
            state.cover_moving = false;
            state.cover_state = if fault == Fault::StoppedShort {
                4
            } else {
                target
            };
        }
        if let Some((at, brightness)) = self.light
            && at <= Instant::now()
        {
            state.calibrator_changing = false;
            state.calibrator_state = 3;
            state.brightness = if fault == Fault::StoppedShort {
                if brightness == 0 { 1 } else { brightness - 1 }
            } else {
                brightness
            };
        }
    }
    pub(super) fn advance(&mut self, state: &CoverCalibratorState) {
        if !state.cover_moving {
            self.cover = None;
        }
        if !state.calibrator_changing {
            self.light = None;
        }
    }
    pub(super) fn write(
        &mut self,
        state: &mut CoverCalibratorState,
        member: &str,
        args: &Values,
    ) -> Result<(), SourceError> {
        match member {
            "opencover" | "closecover" | "haltcover" => {
                if !args.is_empty() {
                    return Err(invalid("Unexpected cover parameters"));
                }
                if state.cover_state == 0 {
                    return Err(unsupported());
                }
                if member == "haltcover" {
                    self.cover = None;
                    // Stopped at an unknown endpoint; do not invent Open/Closed.
                    if state.cover_moving {
                        state.cover_state = 4;
                    }
                    state.cover_moving = false;
                } else {
                    if state.cover_moving {
                        return Err(SourceError::new(
                            ErrorKind::Busy,
                            "Simulated cover is moving",
                        ));
                    }
                    let target = if member == "opencover" { 3 } else { 1 };
                    if state.cover_state != target {
                        self.cover = Some((
                            Instant::now() + Duration::from_secs_f64(state.move_duration_seconds),
                            target,
                        ));
                        state.cover_state = 2;
                        state.cover_moving = true;
                    }
                }
            }
            "calibratoron" => {
                if args.len() != 1 {
                    return Err(invalid("Expected only Brightness"));
                }
                let brightness = args
                    .get("Brightness")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok())
                    .filter(|value| (0..=state.max_brightness).contains(value))
                    .ok_or_else(|| invalid("Invalid simulated brightness"))?;
                if state.calibrator_state == 0 {
                    return Err(unsupported());
                }
                state.brightness = brightness;
                state.calibrator_state = 2;
                state.calibrator_changing = true;
                self.light = Some((
                    Instant::now() + Duration::from_secs_f64(state.light_duration_seconds),
                    brightness,
                ));
            }
            "calibratoroff" => {
                if !args.is_empty() {
                    return Err(invalid("Unexpected light parameters"));
                }
                if state.calibrator_state == 0 {
                    return Err(unsupported());
                }
                self.light = None;
                state.brightness = 0;
                state.calibrator_state = 1;
                state.calibrator_changing = false;
            }
            _ => return Err(unsupported()),
        }
        Ok(())
    }
}
impl CoverCalibratorState {
    pub(super) fn value(
        &self,
        member: &str,
        args: &Values,
        fault: Fault,
    ) -> Result<Value, SourceError> {
        if !args.is_empty() {
            return Err(invalid("Unexpected panel parameters"));
        }
        Ok(match member {
            "brightness" if self.calibrator_state != 0 => json!(self.brightness),
            "maxbrightness" if self.calibrator_state != 0 => json!(self.max_brightness),
            "coverstate" => json!(self.cover_state),
            "calibratorstate" => json!(self.calibrator_state),
            "covermoving" | "calibratorchanging" if fault == Fault::InvalidMotion => json!("false"),
            "covermoving" => json!(self.cover_moving),
            "calibratorchanging" => json!(self.calibrator_changing),
            _ => return Err(unsupported()),
        })
    }
}
