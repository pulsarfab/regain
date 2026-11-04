//! Shared camera-protocol adapter. Bin1 ROI crops a full-sensor RAW16 capture.
use crate::sdk::Sdk;
use anyhow::{Context, Result, ensure};
use regain_core::{Control, Exposure, invalid};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

enum Backend {
    Sdk(Sdk),
    #[cfg(windows)]
    Direct(crate::direct::Camera),
}
impl Backend {
    fn wheel_slots(&self) -> Result<u8> {
        match self {
            Self::Sdk(s) => s.wheel_slots(),
            #[cfg(windows)]
            Self::Direct(c) => {
                let version = c.wheel_query(b"VRS", 8)?.context("Wheel absent")?;
                ensure!(
                    version.len() == 8 && version.iter().all(u8::is_ascii_digit),
                    "Invalid wheel firmware"
                );
                crate::cfw::decode_slots(c.wheel_query(b"MXP", 1)?.context("Wheel absent")?[0])
            }
        }
    }
    fn wheel_position(&self) -> Result<Option<u8>> {
        match self {
            Self::Sdk(s) => s.wheel_position(),
            #[cfg(windows)]
            Self::Direct(c) => c
                .wheel_query(b"NOW", 1)?
                .map(|v| crate::cfw::decode_position(v[0]))
                .transpose(),
        }
    }
    fn wheel_move(&self, position: u8) -> Result<()> {
        match self {
            Self::Sdk(s) => s.wheel_move(position),
            #[cfg(windows)]
            Self::Direct(c) => c.wheel_move(position),
        }
    }
    fn telemetry(&self) -> Result<(f64, f64)> {
        match self {
            Self::Sdk(s) => Ok((s.get(14)?, s.get(15)? * 100. / 255.)),
            #[cfg(windows)]
            Self::Direct(c) => c.temperature_power(),
        }
    }
    fn power(&self, value: f64) -> Result<()> {
        let result = match self {
            Self::Sdk(s) => s.set(16, (value * 255. / 100.).round()),
            #[cfg(windows)]
            Self::Direct(c) => c.manual_power(value),
        };
        result.map_err(|error| {
            regain_core::Failure::UncertainControl {
                message: format!("QHY cooler write unacknowledged: {error:#}"),
                code: None,
            }
            .into()
        })
    }
    fn set(&self, kind: i32, value: i64) -> Result<()> {
        match self {
            Self::Sdk(s) => s.set(
                match kind {
                    0 => 6,
                    5 => 7,
                    6 => 12,
                    _ => return Err(invalid("Unsupported control")),
                },
                value as f64,
            ),
            #[cfg(windows)]
            Self::Direct(c) => match kind {
                0 => c.gain(value as u16),
                5 => c.offset(value as u16),
                6 => c.traffic(value as u8),
                _ => Err(invalid("Unsupported control")),
            },
        }
    }
    fn start(&mut self, us: u32) -> Result<()> {
        match self {
            Self::Sdk(s) => s.start_full_frame(us),
            #[cfg(windows)]
            Self::Direct(c) => c.start(us),
        }
    }
    fn ready(&self) -> Result<bool> {
        match self {
            Self::Sdk(s) => s.ready(),
            #[cfg(windows)]
            Self::Direct(c) => c.ready(),
        }
    }
    fn download(&mut self) -> Result<Vec<u8>> {
        match self {
            Self::Sdk(s) => s.download(),
            #[cfg(windows)]
            Self::Direct(c) => c.download(),
        }
    }
    fn stop(&mut self) -> Result<()> {
        match self {
            Self::Sdk(s) => s.stop(),
            #[cfg(windows)]
            Self::Direct(c) => c.stop(),
        }
    }
}
fn info(serial: &str) -> Value {
    json!({"name":"QHY600M","vendor":"qhy","serial":serial.to_ascii_lowercase(),"width":9600,"height":6422,"pixelSize":3.76,"bitDepth":16,"color":false,"bayer":0,"cooled":true,"shutter":false,"bins":[1],"formats":[2],"retainedFrameReads":false,"hostRegulatedCooling":true})
}
struct Host {
    direct: bool,
    sdk_path: PathBuf,
    backend: Option<Backend>,
    serial: String,
    exposure: Option<Exposure>,
    ready: bool,
    values: BTreeMap<i32, i64>,
    controls: BTreeMap<i32, Control>,
    tick: Instant,
    power_written: Instant,
    power: f64,
    integral: f64,
    max_change: f64,
    cancelled: bool,
    wheel_slots: Option<u8>,
    wheel_expected: Option<u8>,
    wheel_pending: Option<crate::cfw::Motion>,
}
impl Host {
    fn wheel_status(&mut self) -> Result<Value> {
        let backend = self.backend.as_ref().context("Camera not open")?;
        let slots = match self.wheel_slots {
            Some(slots) => slots,
            None => {
                let slots = backend.wheel_slots()?;
                self.wheel_slots = Some(slots);
                slots
            }
        };
        let position = backend.wheel_position()?;
        ensure!(
            position.is_none_or(|p| p < slots),
            invalid("Wheel position outside carousel")
        );
        if let Some(motion) = self.wheel_pending.as_mut()
            && motion.observe(position, Instant::now())?
        {
            self.wheel_pending = None;
        }
        Ok(
            json!({"slots":slots,"position":position.map(i32::from).unwrap_or(-1),"moving":position.is_none() || self.wheel_pending.is_some(),"parentSerial":self.serial}),
        )
    }
    fn verify_wheel(&mut self) -> Result<()> {
        if let Some(expected) = self.wheel_expected {
            let status = self.wheel_status()?;
            ensure!(
                status["moving"] == false && status["position"] == expected,
                invalid("QHY wheel optical state is not verified; exposure blocked")
            );
        }
        Ok(())
    }
    fn list(&self) -> Result<Value> {
        ensure!(
            self.backend.is_none(),
            invalid("Discovery while connected is unavailable")
        );
        #[cfg(windows)]
        if self.direct {
            return Ok(json!(
                regain_transport::cypress::enumerate(
                    0xae18aa60_7f6a_11d4_97dd_00010229b959,
                    0x1618,
                    0xc601
                )?
                .iter()
                .map(|path| {
                    let c = crate::direct::Camera::open(path)?;
                    Ok(info(
                        c.identity["serial"]
                            .as_str()
                            .context("Missing QHY serial")?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?
            ));
        }
        let sdk = Sdk::load(&self.sdk_path)?;
        Ok(json!(
            sdk.list()?
                .into_iter()
                .filter(|c| c.model == "QHY600M")
                .map(|c| info(&c.id))
                .collect::<Vec<_>>()
        ))
    }
    fn observe(&mut self) -> Result<()> {
        let (t, p) = self
            .backend
            .as_ref()
            .context("Camera not open")?
            .telemetry()?;
        ensure!(
            t.is_finite()
                && (-60.0..=80.0).contains(&t)
                && p.is_finite()
                && (0.0..=100.0).contains(&p),
            "Invalid QHY telemetry"
        );
        self.values.insert(8, (t * 10.).round() as i64);
        self.values.insert(15, p.round() as i64);
        Ok(())
    }
    fn tick(&mut self) -> Result<()> {
        if self.backend.is_none() || self.tick.elapsed() < Duration::from_secs(1) {
            return Ok(());
        }
        let dt = self.tick.elapsed().as_secs_f64();
        self.observe()?;
        if self.values[&17] != 0 {
            let power = regain_core::thermal::regulate(
                self.power,
                &mut self.integral,
                self.values[&8] as f64 / 10. - self.values[&16] as f64,
                dt,
                self.max_change,
            );
            // Keep sending steady-state watchdog refreshes, but avoid redundant
            // writes between five-second keepalives when the encoded PWM agrees.
            let encoded = |v: f64| ((v * 255. / 100.).round() as u16) / 2;
            if encoded(power) != encoded(self.power)
                || self.power_written.elapsed() >= Duration::from_secs(5)
            {
                self.backend.as_ref().unwrap().power(power)?;
                self.power_written = Instant::now();
            }
            self.power = power;
        }
        self.tick = Instant::now();
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        if self.exposure.take().is_some() {
            self.ready = false;
            self.cancelled = true;
            self.backend.as_mut().context("Camera not open")?.stop()?;
        }
        Ok(())
    }
    fn close(&mut self) -> Result<()> {
        if self.backend.is_none() {
            return Ok(());
        }
        let stopped = self.stop();
        let off = self.backend.as_ref().unwrap().power(0.);
        self.backend.take();
        self.values.clear();
        self.controls.clear();
        self.cancelled = false;
        self.wheel_slots = None;
        self.wheel_expected = None;
        self.wheel_pending = None;
        // The supervisor may retire the process immediately after the close
        // reply. Preserve the final thermal command and SDK teardown evidence.
        regain_core::diagnostics::flush_usb();
        stopped.and(off)
    }
    fn open(&mut self, p: &Value) -> Result<Value> {
        ensure!(self.backend.is_none(), invalid("Already open"));
        ensure!(p["name"] == "QHY600M", invalid("Unsupported QHY model"));
        let list = self.list()?;
        let found: Vec<_> = list
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| {
                p["serial"].is_null()
                    || p["serial"]
                        .as_str()
                        .zip(c["serial"].as_str())
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            })
            .collect();
        ensure!(
            found.len() == 1,
            invalid("Select exactly one QHY600M serial")
        );
        self.serial = found[0]["serial"].as_str().unwrap().into();
        let backend = if self.direct {
            #[cfg(windows)]
            {
                let paths = regain_transport::cypress::enumerate(
                    0xae18aa60_7f6a_11d4_97dd_00010229b959,
                    0x1618,
                    0xc601,
                )?;
                let mut selected = None;
                for path in paths {
                    let c = crate::direct::Camera::open(&path)?;
                    if c.identity["serial"]
                        .as_str()
                        .is_some_and(|v| v.eq_ignore_ascii_case(&self.serial))
                    {
                        ensure!(selected.is_none(), "Ambiguous QHY serial");
                        selected = Some(c);
                    }
                }
                let mut c = selected.context("QHY600M disappeared")?;
                c.initialize()?;
                Backend::Direct(c)
            }
            #[cfg(not(windows))]
            {
                anyhow::bail!("QHY direct USB currently requires Windows")
            }
        } else {
            let mut s = Sdk::load(&self.sdk_path)?;
            let ids = s.list()?;
            let actual = ids
                .iter()
                .find(|c| c.id.eq_ignore_ascii_case(&self.serial))
                .context("QHY serial disappeared")?;
            s.open(&actual.id, 0)?;
            let chip = s.chip()?;
            ensure!(
                chip.width == 9600 && chip.height == 6422 && chip.bits == 16,
                "Unvalidated QHY geometry"
            );
            Backend::Sdk(s)
        };
        self.backend = Some(backend);
        self.values = BTreeMap::from([
            (0, 30),
            (1, 5_000_000),
            (5, 30),
            (6, 50),
            (8, 0),
            (15, 0),
            (16, 0),
            (17, 0),
        ]);
        if let Some(Backend::Sdk(s)) = &self.backend {
            // The traced SDK defaults match direct initialization, but another
            // SDK build may differ. Report actual values so the supervisor does
            // not incorrectly skip restoring an imaging setting after reopen.
            for (kind, sdk_control, maximum) in [(0, 6, 200.), (5, 7, 255.), (6, 12, 60.)] {
                let value = s.get(sdk_control)?;
                ensure!(
                    (0.0..=maximum).contains(&value) && value.fract() == 0.,
                    "Unexpected QHY SDK control {sdk_control}: {value}"
                );
                self.values.insert(kind, value as i64);
            }
        }
        self.observe()?;
        // New ownership starts with thermal output off. Cooling is explicitly
        // enabled by the frontend after recovering its target and ramp policy.
        self.backend.as_ref().unwrap().power(0.)?;
        self.power = 0.;
        self.power_written = Instant::now();
        self.integral = 0.;
        self.tick = Instant::now();
        self.cancelled = false;
        self.controls = [
            (0, 0, 200, true),
            (1, 1, 3_600_000_000, true),
            (5, 0, 255, true),
            (6, 0, 60, true),
            (8, -600, 800, false),
            (15, 0, 100, false),
            (16, -40, 30, true),
            (17, 0, 1, true),
        ]
        .into_iter()
        .map(|(kind, min, max, writable)| {
            (
                kind,
                Control {
                    kind,
                    min,
                    max,
                    writable,
                    value: self.values[&kind],
                },
            )
        })
        .collect();
        let version = match self.backend.as_ref().unwrap() {
            Backend::Sdk(s) => format!("{:?}", s.version()?),
            #[cfg(windows)]
            _ => "QHY600 direct".into(),
        };
        Ok(
            json!({"info":info(&self.serial),"serial":self.serial,"sdkVersion":version,"controls":self.controls.values().collect::<Vec<_>>(),"backend":if self.direct{"direct"}else{"sdk"}}),
        )
    }
    fn command(&mut self, method: &str, p: Value) -> Result<(Value, Vec<u8>)> {
        let mut pixels = Vec::new();
        let value = match method {
            "wheel-status" => self.wheel_status()?,
            "wheel-move" => {
                ensure!(
                    self.exposure.is_none(),
                    invalid("Cannot move wheel during acquisition")
                );
                let status = self.wheel_status()?;
                ensure!(
                    status["moving"] == false,
                    invalid("Wheel is already moving or unknown")
                );
                let position = p["position"]
                    .as_u64()
                    .filter(|v| *v < u64::from(self.wheel_slots.unwrap()))
                    .context("Invalid wheel target")? as u8;
                // Intent is recorded before dispatch; an error can mean motion
                // started. Neither worker nor supervisor replays the write.
                self.wheel_expected = Some(position);
                if status["position"] == position {
                    return Ok((status, pixels));
                }
                self.wheel_pending = Some(crate::cfw::Motion::new(position, Instant::now()));
                self.backend.as_ref().unwrap().wheel_move(position)?;
                json!({"slots":self.wheel_slots,"position":-1,"moving":true,"parentSerial":self.serial})
            }
            "list" => self.list()?,
            "open" => self.open(&p)?,
            "runtime-options" => {
                let o: regain_core::camera_options::CameraRuntimeOptions =
                    serde_json::from_value(p)?;
                o.validate()?;
                crate::diagnostics::configure(&o.transfer_log_mode)?;
                self.max_change = o.cooler_max_power_change_per_5_seconds;
                serde_json::to_value(o)?
            }
            "resume-cooling" => {
                let power = p["power"].as_f64().context("Missing prior power")?;
                let prior = p["temperature"]
                    .as_f64()
                    .context("Missing prior temperature")?;
                let previous = p["previousTarget"]
                    .as_f64()
                    .context("Missing prior target")?;
                ensure!(
                    (0.0..=100.0).contains(&power)
                        && (-60.0..=80.0).contains(&prior)
                        && (-40.0..=30.0).contains(&previous),
                    invalid("Invalid cooling recovery seed")
                );
                self.observe()?;
                if self.values[&17] != 0 {
                    let (demand, integral) = regain_core::thermal::recovery_state(
                        power,
                        prior,
                        self.values[&8] as f64 / 10.,
                        self.values[&16] as f64,
                        previous,
                    );
                    let demand = if self.values[&16] as f64 > previous {
                        0.
                    } else {
                        regain_core::thermal::limit_power(
                            power,
                            demand,
                            self.tick.elapsed().as_secs_f64(),
                            self.max_change,
                        )
                    };
                    self.backend.as_ref().unwrap().power(demand)?;
                    self.power = demand;
                    self.power_written = Instant::now();
                    self.integral = integral;
                    self.tick = Instant::now();
                }
                Value::Null
            }
            "get" | "get-observation" => {
                let k = p["control"]
                    .as_i64()
                    .and_then(|v| i32::try_from(v).ok())
                    .context("Missing control")?;
                ensure!(self.backend.is_some(), invalid("Camera not open"));
                if matches!(k, 8 | 15) {
                    self.observe()?;
                }
                let value = self
                    .values
                    .get(&k)
                    .ok_or_else(|| invalid("Unsupported QHY control"))?;
                if method == "get" {
                    json!(value)
                } else {
                    json!({"value":value,"ageSeconds":0.0})
                }
            }
            "set" => {
                let k = p["control"]
                    .as_i64()
                    .and_then(|v| i32::try_from(v).ok())
                    .context("Missing control")?;
                let v = p["value"].as_i64().context("Missing value")?;
                let cap = self
                    .controls
                    .get(&k)
                    .ok_or_else(|| invalid("Unsupported QHY control"))?;
                ensure!(
                    cap.writable && (cap.min..=cap.max).contains(&v),
                    invalid("QHY control outside range")
                );
                ensure!(
                    matches!(k, 16 | 17) || self.exposure.is_none(),
                    invalid("Camera is exposing")
                );
                match k {
                    0 | 5 | 6 => self
                        .backend
                        .as_ref()
                        .context("Camera not open")?
                        .set(k, v)?,
                    16 => {}
                    17 => {
                        if v == 0 {
                            self.backend
                                .as_ref()
                                .context("Camera not open")?
                                .power(0.)?;
                            self.power = 0.;
                            self.integral = 0.;
                        }
                    }
                    _ => return Err(invalid("Unsupported persistent QHY control")),
                }
                self.values.insert(k, v);
                Value::Null
            }
            "validate" | "start" => {
                let e: Exposure = serde_json::from_value(p.clone())?;
                regain_core::validate_exposure(&info(&self.serial), &self.controls, &e)?;
                ensure!(e.bin == 1, invalid("Only bin1 is validated for QHY"));
                if method == "start" {
                    if !p["wheelExpected"].is_null() {
                        let expected = &p["wheelExpected"];
                        let status = self.wheel_status()?;
                        ensure!(
                            status["slots"] == expected["slots"],
                            invalid("QHY wheel geometry changed")
                        );
                        self.wheel_expected = Some(
                            expected["position"]
                                .as_u64()
                                .filter(|v| *v < self.wheel_slots.unwrap() as u64)
                                .context("Invalid expected wheel position")?
                                as u8,
                        );
                        self.verify_wheel()?;
                    }
                    ensure!(
                        self.exposure.is_none(),
                        invalid("Prior acquisition is still active")
                    );
                    if self.cancelled {
                        // Only an explicitly cancelled/failed acquisition needs
                        // reinitialization. Successful frames keep the SDK cadence.
                        #[cfg(windows)]
                        if let Some(Backend::Direct(c)) = self.backend.as_mut() {
                            c.initialize()?;
                        }
                        for kind in [0, 5, 6] {
                            self.backend
                                .as_ref()
                                .context("Camera not open")?
                                .set(kind, self.values[&kind])?;
                        }
                        self.cancelled = false;
                    }
                    // Arm cleanup before dispatch: a failed start acknowledgement
                    // does not prove that the sensor remained idle.
                    self.exposure = Some(e.clone());
                    self.ready = false;
                    self.backend
                        .as_mut()
                        .context("Camera not open")?
                        .start(u32::try_from(e.microseconds)?)?;
                }
                Value::Null
            }
            "status" => {
                if self.cancelled {
                    json!(3)
                } else if self.exposure.is_none() {
                    json!(0)
                } else {
                    self.tick()?;
                    self.ready = self.backend.as_ref().unwrap().ready()?;
                    json!(if self.ready { 2 } else { 1 })
                }
            }
            "download" => {
                let e = self.exposure.as_ref().context("No QHY exposure")?.clone();
                ensure!(self.ready, "QHY frame not ready");
                self.ready = false;
                self.cancelled = true;
                let full = self.backend.as_mut().unwrap().download()?;
                self.verify_wheel()?;
                pixels = crate::image::crop_raw16(
                    full,
                    9600,
                    6422,
                    e.x as usize,
                    e.y as usize,
                    e.width as usize,
                    e.height as usize,
                )?;
                self.exposure = None;
                self.cancelled = false;
                json!({"width":e.width,"height":e.height,"bytes":pixels.len(),"vendor":"qhy","hardwareReadMode":0})
            }
            "stop" => {
                self.stop()?;
                Value::Null
            }
            "close" => {
                self.close()?;
                Value::Null
            }
            "diagnostics-dump" => {
                crate::diagnostics::dump("Manual QHY diagnostic snapshot");
                Value::Null
            }
            _ => return Err(invalid(format!("Unsupported QHY command {method}"))),
        };
        Ok((value, pixels))
    }
}
pub fn run(direct: bool, args: Vec<String>) -> Result<()> {
    ensure!(
        !args.iter().any(|a| a == "--simulate"),
        "QHY driver requires physical hardware"
    );
    let sdk_path = args
        .windows(2)
        .find(|v| v[0] == "--sdk")
        .map(|v| PathBuf::from(&v[1]))
        .unwrap_or_else(Sdk::default_path);
    let mut h = Host {
        direct,
        sdk_path,
        backend: None,
        serial: String::new(),
        exposure: None,
        ready: false,
        values: BTreeMap::new(),
        controls: BTreeMap::new(),
        tick: Instant::now(),
        power_written: Instant::now(),
        power: 0.,
        integral: 0.,
        max_change: 25.,
        cancelled: false,
        wheel_slots: None,
        wheel_expected: None,
        wheel_pending: None,
    };
    regain_worker::camera::serve(
        &mut h,
        |h, m, p| {
            let result = h.command(m, p);
            if let Err(error) = &result {
                crate::diagnostics::dump(&format!("{m}: {error:#}"));
            }
            result
        },
        |h| {
            let result = h.tick();
            if let Err(error) = &result {
                crate::diagnostics::dump(&format!("Thermal owner: {error:#}"));
            }
            result
        },
        |error| {
            json!({"sdkCode":if matches!(error.downcast_ref::<regain_core::Failure>(), Some(regain_core::Failure::Invalid(_))) { Some(8) } else { None },
            "controlUncertain":matches!(error.downcast_ref::<regain_core::Failure>(), Some(regain_core::Failure::UncertainControl{..}))})
        },
    )
}
impl Drop for Host {
    fn drop(&mut self) {
        if let Err(e) = self.close() {
            crate::diagnostics::dump(&format!("Close: {e:#}"));
        }
    }
}
