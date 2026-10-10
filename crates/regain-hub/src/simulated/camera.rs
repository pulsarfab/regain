//! Explicit camera test state. Acquisition ownership, publication and image
//! retention still belong to the ordinary source actor and camera supervisor.
use super::{Fault, invalid, unsupported};
use crate::{
    camera::{
        acquisition::GuideRequest,
        image::{CameraImage, ElementType, ImageBudget, ImageDescriptor, ImageOrder},
        properties::CameraSetting,
    },
    source::{ErrorKind, SourceError, Values},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tokio::time::Instant;

const WIDTH: i32 = 320;
const HEIGHT: i32 = 240;
const MAX_BIN: i32 = 4;
const MODES: [&str; 3] = ["Mono 16", "RGB 16", "Mono 16 (rank three)"];

// Simulation status contains only injected controls. Ordinary settings and
// acquisition state are read through the common typed camera diagnostics.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraState {
    #[serde(skip)]
    pub bin_x: i32,
    #[serde(skip)]
    pub bin_y: i32,
    #[serde(skip)]
    pub num_x: i32,
    #[serde(skip)]
    pub num_y: i32,
    #[serde(skip)]
    pub start_x: i32,
    #[serde(skip)]
    pub start_y: i32,
    #[serde(skip)]
    pub gain: i32,
    #[serde(skip)]
    pub offset: i32,
    #[serde(skip)]
    pub readout_mode: i32,
    #[serde(skip)]
    pub fast_readout: bool,
    #[serde(skip)]
    pub cooler_on: bool,
    #[serde(skip)]
    pub set_ccd_temperature: f64,
    pub temperature: f64,
    pub heat_sink_temperature: f64,
    pub readout_duration_seconds: f64,
    pub can_abort_exposure: bool,
    pub can_stop_exposure: bool,
    pub can_pulse_guide: bool,
    pub can_fast_readout: bool,
    pub can_set_ccd_temperature: bool,
    pub can_get_cooler_power: bool,
    pub has_shutter: bool,
    pub temperature_available: bool,
    pub exposure_metadata_available: bool,
    #[serde(skip)]
    pub image_ready: bool,
    #[serde(skip)]
    pub camera_state: i32,
    #[serde(skip)]
    pub percent_completed: i32,
}
impl Default for CameraState {
    fn default() -> Self {
        Self {
            bin_x: 1,
            bin_y: 1,
            num_x: WIDTH,
            num_y: HEIGHT,
            start_x: 0,
            start_y: 0,
            gain: 100,
            offset: 10,
            readout_mode: 0,
            fast_readout: false,
            cooler_on: false,
            set_ccd_temperature: -10.0,
            temperature: 12.0,
            heat_sink_temperature: 20.0,
            readout_duration_seconds: 0.2,
            can_abort_exposure: true,
            can_stop_exposure: true,
            can_pulse_guide: false,
            can_fast_readout: true,
            can_set_ccd_temperature: true,
            can_get_cooler_power: true,
            has_shutter: true,
            temperature_available: true,
            exposure_metadata_available: true,
            image_ready: false,
            camera_state: 0,
            percent_completed: 0,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraUpdate {
    #[schemars(range(min = 0, max = 300))]
    pub readout_duration_seconds: Option<f64>,
    #[schemars(range(min = -273.15))]
    pub temperature: Option<f64>,
    #[schemars(range(min = -273.15))]
    pub heat_sink_temperature: Option<f64>,
    pub can_abort_exposure: Option<bool>,
    pub can_stop_exposure: Option<bool>,
    pub can_pulse_guide: Option<bool>,
    pub can_fast_readout: Option<bool>,
    pub can_set_ccd_temperature: Option<bool>,
    pub can_get_cooler_power: Option<bool>,
    pub has_shutter: Option<bool>,
    pub temperature_available: Option<bool>,
    pub exposure_metadata_available: Option<bool>,
}
impl CameraUpdate {
    pub(super) fn apply(self, state: &mut CameraState) -> Result<(), SourceError> {
        macro_rules! apply { ($($key:ident),*) => { $(if let Some(value) = self.$key { state.$key = value; })* }; }
        apply!(
            readout_duration_seconds,
            temperature,
            heat_sink_temperature,
            can_abort_exposure,
            can_stop_exposure,
            can_pulse_guide,
            can_fast_readout,
            can_set_ccd_temperature,
            can_get_cooler_power,
            has_shutter,
            temperature_available,
            exposure_metadata_available
        );
        if !state.readout_duration_seconds.is_finite()
            || !(0.0..=300.0).contains(&state.readout_duration_seconds)
            || [state.temperature, state.heat_sink_temperature]
                .iter()
                .any(|value| !value.is_finite() || *value < -273.15)
            || state.fast_readout && !state.can_fast_readout
            || state.cooler_on && !state.can_set_ccd_temperature
        {
            return Err(invalid("Invalid simulated camera state"));
        }
        Ok(())
    }
}

struct Exposure {
    completed: AtomicBool,
    started: Instant,
    started_utc: String,
    duration: Duration,
    readout: Duration,
    settings: CameraState,
    light: bool,
    sequence: u64,
}
#[derive(Default)]
pub(super) struct CameraExposure {
    exposure: Option<Exposure>,
    guide: Option<(Instant, Duration)>,
    sequence: u64,
}
impl CameraExposure {
    pub(super) fn effective(&self, state: &mut CameraState, fault: Fault) {
        state.image_ready = false;
        state.camera_state = 0;
        state.percent_completed = 0;
        if let Some(exposure) = &self.exposure {
            let elapsed = exposure.started.elapsed();
            let done = elapsed >= exposure.duration;
            if done
                && elapsed >= exposure.duration + exposure.readout
                && fault != Fault::StalledExposure
            {
                exposure.completed.store(true, Ordering::Relaxed);
            }
            // A fault armed for the next acquisition cannot reopen a completed
            // frame or change its metadata/readiness retroactively.
            state.image_ready = exposure.completed.load(Ordering::Relaxed);
            state.camera_state = if state.image_ready {
                0
            } else if done {
                3
            } else {
                2
            };
            state.percent_completed = if done || exposure.duration.is_zero() {
                100
            } else {
                (100.0 * elapsed.as_secs_f64() / exposure.duration.as_secs_f64()) as i32
            };
        }
    }
    pub(super) fn value(
        &self,
        state: &CameraState,
        member: &str,
        args: &Values,
    ) -> Result<Value, SourceError> {
        if !args.is_empty() {
            return Err(invalid("Unexpected camera parameters"));
        }
        let value = match member {
            "cameraxsize" => json!(WIDTH),
            "cameraysize" => json!(HEIGHT),
            "maxbinx" | "maxbiny" => json!(MAX_BIN),
            "binx" => json!(state.bin_x),
            "biny" => json!(state.bin_y),
            "numx" => json!(state.num_x),
            "numy" => json!(state.num_y),
            "startx" => json!(state.start_x),
            "starty" => json!(state.start_y),
            "bayeroffsetx" | "bayeroffsety" if state.readout_mode == 1 => json!(0),
            "camerastate" => json!(state.camera_state),
            "imageready" => json!(state.image_ready),
            "percentcompleted" => json!(state.percent_completed),
            "canasymmetricbin" => json!(true),
            "canpulseguide" => json!(state.can_pulse_guide),
            "ispulseguiding" if state.can_pulse_guide => json!(self.guiding()),
            "canabortexposure" => json!(state.can_abort_exposure),
            "canstopexposure" => json!(state.can_stop_exposure),
            "canfastreadout" => json!(state.can_fast_readout),
            "cangetcoolerpower" => json!(state.can_get_cooler_power),
            "cansetccdtemperature" => json!(state.can_set_ccd_temperature),
            "hasshutter" => json!(state.has_shutter),
            "fastreadout" if state.can_fast_readout => json!(state.fast_readout),
            "cooleron" if state.can_set_ccd_temperature => json!(state.cooler_on),
            "coolerpower" if state.can_get_cooler_power => {
                json!(if state.cooler_on { 37.5 } else { 0.0 })
            }
            "setccdtemperature" if state.can_set_ccd_temperature => {
                json!(state.set_ccd_temperature)
            }
            "ccdtemperature" if state.temperature_available => json!(state.temperature),
            "heatsinktemperature" if state.temperature_available => {
                json!(state.heat_sink_temperature)
            }
            "electronsperadu" => json!(0.8),
            "fullwellcapacity" => json!(50000.0),
            "exposuremin" | "exposureresolution" => json!(0.001),
            "exposuremax" => json!(3600.0),
            "gain" => json!(state.gain),
            "gainmin" => json!(0),
            "gainmax" => json!(600),
            "offset" => json!(state.offset),
            "offsetmin" => json!(0),
            "offsetmax" => json!(255),
            "maxadu" => json!(65535),
            "pixelsizex" | "pixelsizey" => json!(3.76),
            "readoutmode" => json!(state.readout_mode),
            "readoutmodes" => json!(MODES),
            "sensorname" => json!("Regain simulated sensor"),
            "sensortype" => json!(if state.readout_mode == 1 { 1 } else { 0 }),
            "lastexposureduration" | "lastexposurestarttime"
                if state.exposure_metadata_available =>
            {
                let exposure = self
                    .exposure
                    .as_ref()
                    .filter(|_| state.image_ready)
                    .ok_or_else(|| {
                        SourceError::new(ErrorKind::Unavailable, "No completed simulated exposure")
                    })?;
                if member == "lastexposureduration" {
                    json!(exposure.duration.as_secs_f64())
                } else {
                    json!(exposure.started_utc)
                }
            }
            _ => return Err(unsupported()),
        };
        Ok(value)
    }
    pub(super) fn write(
        &mut self,
        state: &mut CameraState,
        member: &str,
        args: &Values,
    ) -> Result<(), SourceError> {
        match member {
            "pulseguide" => {
                let request = GuideRequest::from_parameters(args)?;
                if !state.can_pulse_guide {
                    return Err(unsupported());
                }
                if self.guiding() {
                    return Err(SourceError::new(
                        ErrorKind::Busy,
                        "Simulated camera is guiding",
                    ));
                }
                self.guide = Some((
                    Instant::now(),
                    Duration::from_millis(request.duration_milliseconds as u64),
                ));
            }
            "startexposure" => {
                if args.len() != 2 {
                    return Err(invalid("Expected Duration and Light"));
                }
                let seconds = args
                    .get("Duration")
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite() && (0.0..=3600.0).contains(value))
                    .ok_or_else(|| invalid("Invalid simulated exposure duration"))?;
                let light = args
                    .get("Light")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| invalid("Expected boolean Light"))?;
                if light && seconds < 0.001 {
                    return Err(invalid("Light exposure is shorter than ExposureMin"));
                }
                if state.camera_state != 0 {
                    return Err(SourceError::new(
                        ErrorKind::Busy,
                        "Simulated camera is acquiring",
                    ));
                }
                if (i64::from(state.start_x) + i64::from(state.num_x)) * i64::from(state.bin_x)
                    > i64::from(WIDTH)
                    || (i64::from(state.start_y) + i64::from(state.num_y)) * i64::from(state.bin_y)
                        > i64::from(HEIGHT)
                {
                    return Err(invalid("Simulated subframe exceeds sensor bounds"));
                }
                let sequence = self
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| invalid("Simulated exposure sequence exhausted"))?;
                self.exposure = Some(Exposure {
                    completed: AtomicBool::new(false),
                    started: Instant::now(),
                    // Wall time is descriptive metadata only. Integration,
                    // readiness, readout and Halt/Stop still use monotonic time.
                    started_utc: chrono::Utc::now()
                        .format("%Y-%m-%dT%H:%M:%S%.9f")
                        .to_string(),
                    duration: Duration::from_secs_f64(seconds),
                    readout: Duration::from_secs_f64(state.readout_duration_seconds),
                    settings: state.clone(),
                    light,
                    sequence,
                });
                self.sequence = sequence;
            }
            "stopexposure" | "abortexposure" => {
                if !args.is_empty() {
                    return Err(invalid("Unexpected exposure control parameters"));
                }
                if member == "abortexposure" {
                    if !state.can_abort_exposure {
                        return Err(unsupported());
                    }
                    self.exposure = None;
                } else {
                    if !state.can_stop_exposure {
                        return Err(unsupported());
                    }
                    if let Some(exposure) = self.exposure.as_mut() {
                        exposure.duration = exposure.started.elapsed().min(exposure.duration);
                    }
                }
            }
            _ => {
                let setting = CameraSetting::from_parameters(member, args)?;
                if setting.changes_capture() && state.camera_state != 0 {
                    return Err(SourceError::new(
                        ErrorKind::Busy,
                        "Simulated camera is acquiring",
                    ));
                }
                match setting {
                    CameraSetting::BinX(value) if value <= MAX_BIN => state.bin_x = value,
                    CameraSetting::BinY(value) if value <= MAX_BIN => state.bin_y = value,
                    // Desired ROI may be temporarily outside the sensor while
                    // clients change several fields. StartExposure checks the
                    // complete geometry before replacing state or allocating.
                    CameraSetting::NumX(value) => state.num_x = value,
                    CameraSetting::NumY(value) => state.num_y = value,
                    CameraSetting::StartX(value) => state.start_x = value,
                    CameraSetting::StartY(value) => state.start_y = value,
                    CameraSetting::Gain(value) if (0..=600).contains(&value) => state.gain = value,
                    CameraSetting::Offset(value) if (0..=255).contains(&value) => {
                        state.offset = value
                    }
                    CameraSetting::ReadoutMode(value)
                        if (0..MODES.len() as i32).contains(&value) =>
                    {
                        state.readout_mode = value
                    }
                    CameraSetting::FastReadout(value) if state.can_fast_readout => {
                        state.fast_readout = value
                    }
                    CameraSetting::CoolerOn(value) if state.can_set_ccd_temperature => {
                        state.cooler_on = value
                    }
                    CameraSetting::SetCcdTemperature(value)
                        if state.can_set_ccd_temperature && value <= 60.0 =>
                    {
                        state.set_ccd_temperature = value
                    }
                    CameraSetting::SubExposureDuration(_) => return Err(unsupported()),
                    CameraSetting::FastReadout(_) if !state.can_fast_readout => {
                        return Err(unsupported());
                    }
                    CameraSetting::CoolerOn(_) | CameraSetting::SetCcdTemperature(_)
                        if !state.can_set_ccd_temperature =>
                    {
                        return Err(unsupported());
                    }
                    _ => return Err(invalid("Simulated camera setting is outside its limits")),
                }
            }
        }
        Ok(())
    }
    fn guiding(&self) -> bool {
        self.guide
            .as_ref()
            .is_some_and(|(started, duration)| started.elapsed() < *duration)
    }
    pub(super) fn image(
        &self,
        state: &CameraState,
        budget: ImageBudget,
        fault: Fault,
    ) -> Result<CameraImage, SourceError> {
        if !state.image_ready {
            return Err(SourceError::new(
                ErrorKind::Unavailable,
                "Simulated image is not ready",
            ));
        }
        if fault == Fault::ImageError {
            return Err(SourceError::new(
                ErrorKind::Unavailable,
                "Injected simulated image failure",
            ));
        }
        let exposure = self.exposure.as_ref().ok_or_else(unsupported)?;
        let settings = &exposure.settings;
        let descriptor = ImageDescriptor::new(
            settings.num_x as u32 + u32::from(fault == Fault::InvalidImage),
            settings.num_y as u32,
            match settings.readout_mode {
                1 => Some(3),
                2 => Some(1),
                _ => None,
            },
            ElementType::Int32,
            ElementType::UInt16,
            ImageOrder::SensorRows,
        )?;
        // Host budget admission happens before an image-sized allocation.
        let mut allocation = budget.allocate(descriptor)?;
        let planes = descriptor.planes().unwrap_or(1) as usize;
        for (index, pixel) in allocation
            .bytes_mut()
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .enumerate()
        {
            let plane = index % planes;
            let x = (index / planes % descriptor.width() as usize + settings.start_x as usize)
                * settings.bin_x as usize;
            let y = (index / planes / descriptor.width() as usize + settings.start_y as usize)
                * settings.bin_y as usize;
            let signal = if exposure.light {
                x * 17 + y * 31 + plane * 1009 + settings.gain as usize
            } else {
                0
            };
            let value = (signal as u64 + exposure.sequence + settings.offset as u64) as u16;
            *pixel = value.to_le_bytes();
        }
        Ok(allocation.finish())
    }
}

pub(super) fn controls() -> Vec<Value> {
    let state = serde_json::to_value(CameraState::default()).unwrap();
    let mut fields = vec![
        json!({"path":["camera","readoutDurationSeconds"],"type":"number",
        "label":"Readout duration (s)","description":"New exposures enter Reading for this monotonic duration after integration. Settings are frozen at StartExposure.",
        "default":state["readoutDurationSeconds"],"minimum":0.0,"maximum":300.0}),
    ];
    for (key, label) in [
        ("temperature", "CCD temperature (°C)"),
        ("heatSinkTemperature", "Heat sink temperature (°C)"),
    ] {
        fields.push(json!({"path":["camera",key],"type":"number","label":label,
            "description":"Injected reading; cooler controls do not simulate thermal dynamics.","default":state[key],"minimum":-273.15}));
    }
    for (key, label) in [
        ("canAbortExposure", "Abort available"),
        ("canStopExposure", "Stop available"),
        ("canPulseGuide", "Pulse guiding available"),
        ("canFastReadout", "Fast readout available"),
        ("canSetCcdTemperature", "Cooler control available"),
        ("canGetCoolerPower", "Cooler power available"),
        ("hasShutter", "Shutter present"),
        ("temperatureAvailable", "Temperature available"),
        ("exposureMetadataAvailable", "Exposure metadata available"),
    ] {
        fields.push(json!({"path":["camera",key],"type":"boolean","label":label,
            "description":"Inject an optional camera capability. Unsupported properties remain explicit errors.","default":state[key]}));
    }
    fields
}
