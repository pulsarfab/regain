//! Version-1 plugin protocol over inherited pipes for the verified camera interfaces.
//! A dedicated worker owns the exclusive driver handle for the entire connection.
use crate::{asi220, asi676, asi2600, asi6200, settings::Settings, transport};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    sync::mpsc,
    time::{Duration, Instant},
};

type Frame = (Value, Vec<u8>);
enum Work {
    Capture(
        Settings,
        i32,
        u32,
        Duration,
        Duration,
        bool,
        mpsc::SyncSender<Result<Frame>>,
    ),
    Environment(u32, Option<i64>, mpsc::SyncSender<Result<i64>>),
}
#[derive(Clone, Copy, Default, PartialEq)]
enum Model {
    #[default]
    Asi676,
    Duo,
    Guide,
    Asi6200,
    Asi2600P25,
}
impl Model {
    const ALL: [Self; 5] = [
        Self::Asi676,
        Self::Duo,
        Self::Guide,
        Self::Asi6200,
        Self::Asi2600P25,
    ];
    fn cooled(self) -> bool {
        matches!(self, Self::Duo | Self::Asi6200 | Self::Asi2600P25)
    }
    fn name(self) -> &'static str {
        match self {
            Self::Asi676 => "ZWO ASI676MC",
            Self::Duo => "ZWO ASI2600MM Duo",
            Self::Guide => "ZWO ASI220MM Mini",
            Self::Asi6200 => "ZWO ASI6200MM Pro",
            Self::Asi2600P25 => "ZWO ASI2600MM Pro",
        }
    }
    fn pid(self) -> u32 {
        match self {
            Self::Asi676 => 0x676d,
            Self::Duo => 0x2601,
            Self::Guide => 0x2209,
            Self::Asi6200 => 0x620b,
            Self::Asi2600P25 => 0x260e,
        }
    }
    fn descriptor(self) -> Value {
        let (width, height, pixel, bits, bins, alignment) = match self {
            Self::Asi676 => (3552, 3552, 2.0, 12, vec![1], 2),
            Self::Duo | Self::Asi2600P25 => (6248, 4176, 3.76, 16, vec![1, 2, 3, 4], 16),
            Self::Guide => (1920, 1080, 4.0, 12, vec![1, 2], 2),
            Self::Asi6200 => (9576, 6388, 3.76, 16, vec![1, 2, 3, 4], 16),
        };
        json!({"id":self.pid(),"name":self.name(),"width":width,"height":height,"color":self == Self::Asi676,"bayer":0,
            "pixelSize":pixel,"bitDepth":bits,"cooled":self.cooled(),"shutter":false,"bins":bins,"formats":[2],
            "minimumWidth":64,"minimumHeight":64,"originAlignment":alignment,
            "retainedFrameReads":self != Self::Guide})
    }
    fn controls(self) -> Vec<Value> {
        let (gain_min, gain_max, offset_min, offset_max, offset_default, exp_max) = match self {
            Self::Asi676 => (0, 600, 0, 200, 10, 30_000_000),
            Self::Duo => (-25, 700, 0, 240, 50, asi2600::MAX_EXPOSURE_US as i32),
            Self::Asi2600P25 => (-25, 700, 0, 240, 1, asi2600::MAX_EXPOSURE_US as i32),
            Self::Guide => (0, 600, 200, 1500, 200, 10_000_000),
            Self::Asi6200 => (0, 700, 0, 200, 50, asi6200::MAX_EXPOSURE_US as i32),
        };
        let mut caps = vec![];
        for (kind, min, max, value, writable) in [
            (0, gain_min, gain_max, 0, true),
            (1, 32, exp_max, 100000, true),
            (5, offset_min, offset_max, offset_default, true),
            (6, 40, 40, 40, false),
        ] {
            caps.push(json!({"type":kind,"min":min,"max":max,"value":value,"writable":writable}));
        }
        if self.cooled() {
            for (kind, min, max, value, writable) in [
                (8, -500, 850, 250, false),
                (15, 0, 100, 0, false),
                (16, -40, 30, 25, true),
                (17, 0, 1, 0, true),
                (21, 0, 1, 0, true),
            ] {
                caps.push(
                    json!({"type":kind,"min":min,"max":max,"value":value,"writable":writable}),
                );
            }
        }
        if matches!(self, Self::Asi6200 | Self::Asi2600P25) {
            for kind in [22, 23] {
                caps.push(json!({"type":kind,"min":0,"max":255,"value":255,"writable":true}));
            }
        }
        caps
    }
    fn validate(self, settings: &Settings, gain: i32, bin: u32) -> Result<()> {
        match self {
            Self::Asi676 => {
                ensure!(bin == 1, "ASI676MC direct capture supports bin 1 only");
                settings.validate()
            }
            Self::Duo | Self::Asi2600P25 => asi2600::raw_settings(settings, gain, bin).map(|_| ()),
            Self::Guide => asi220::raw_settings(settings, bin).map(|_| ()),
            Self::Asi6200 => asi6200::raw_settings(settings, gain, bin).map(|_| ()),
        }
    }
}
fn paths(model: Model) -> Result<Vec<transport::DeviceInfo>> {
    Ok(transport::enumerate()?
        .into_iter()
        .filter(|path| path.matches(0x03c3, model.pid() as u16))
        .collect())
}

#[derive(Debug)]
struct HardwareFailure(String);
impl std::fmt::Display for HardwareFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for HardwareFailure {}
fn hardware(error: anyhow::Error) -> anyhow::Error {
    error.context(HardwareFailure("direct camera operation failed".into()))
}

fn open_camera(model: Model, serial: Option<&str>) -> Result<(transport::Camera, Value, String)> {
    let paths = paths(model)?;
    ensure!(!paths.is_empty(), "selected direct camera is not attached");
    ensure!(
        serial.is_some() || paths.len() == 1,
        "multiple cameras of this model require a serial number"
    );
    let (camera, info, found) = find_accessible(paths, |path| {
        let camera = transport::Camera::open(&path)?;
        let info = camera.probe()?;
        ensure!(
            info["productId"] == model.pid()
                && info["usbVersionBcd"] == if model == Model::Guide { 0x200 } else { 0x300 },
            "camera USB interface differs from the verified model"
        );
        // SDK 1.41 ASIGetSerialNumber uses vendor IN C8, value/index 0, eight bytes.
        let bytes = camera.vendor(0xc8, 0, 0, 8)?;
        ensure!(
            bytes.iter().any(|&v| v != 0),
            "camera serial is unavailable"
        );
        let found: String = bytes.iter().map(|v| format!("{v:02x}")).collect();
        if serial.is_none_or(|s| s == found) {
            return Ok(Some((camera, info, found)));
        }
        Ok(None)
    })?;
    if model.cooled() {
        camera.enable_environment(matches!(model, Model::Asi6200 | Model::Asi2600P25))?;
    }
    Ok((camera, info, found))
}

fn find_accessible<I: IntoIterator, T>(
    candidates: I,
    mut inspect: impl FnMut(I::Item) -> Result<Option<T>>,
) -> Result<T> {
    let mut last_error = None;
    for candidate in candidates {
        match inspect(candidate) {
            Ok(Some(found)) => return Ok(found),
            Ok(None) => {}
            Err(error) => {
                crate::diagnostics::log(
                    "warning",
                    "camera.unavailable",
                    format_args!("Skipping camera candidate: {error:#}"),
                );
                last_error = Some(error);
            }
        }
    }
    if let Some(error) = last_error {
        return Err(error.context("selected serial could not be found among accessible cameras"));
    }
    bail!("selected camera serial is not attached")
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn hardware_classification_preserves_transport_details() {
        let error = hardware(crate::transfer::Failure::new("timeout", 1024, 0, true).into());
        assert!(error.is::<HardwareFailure>());
        assert_eq!(
            crate::transfer::Failure::details(&error)["category"],
            "timeout"
        );
    }
    #[test]
    fn status_and_download_keep_the_original_transport_failure() {
        let mut host = Host {
            simulate: true,
            ..Host::default()
        };
        host.command("open", &json!({"name":"ZWO ASI2600MM Pro"}))
            .unwrap();
        host.frame = Some(Err(crate::transfer::Failure::new(
            "short_read",
            1048576,
            512,
            false,
        )
        .into()));
        for method in ["status", "download"] {
            let error = host.command(method, &Value::Null).unwrap_err();
            assert!(error.is::<HardwareFailure>());
            assert_eq!(
                crate::transfer::Failure::details(&error)["receivedBytes"],
                512
            );
        }
        host.command("close", &Value::Null).unwrap();
    }
    #[test]
    fn skips_busy_and_nonmatching_devices_but_never_substitutes_them() {
        let mut visited = vec![];
        let found = find_accessible([0, 1, 2], |id| {
            visited.push(id);
            match id {
                0 => bail!("busy"),
                1 => Ok(None),
                _ => Ok(Some(id)),
            }
        })
        .unwrap();
        assert_eq!(found, 2);
        assert_eq!(visited, [0, 1, 2]);
        assert!(
            find_accessible([0, 1], |id| -> Result<Option<i32>> {
                if id == 0 { bail!("busy") } else { Ok(None) }
            })
            .is_err()
        );
    }
}

struct Worker {
    sender: mpsc::Sender<Work>,
    thread: Option<std::thread::JoinHandle<()>>,
    telemetry: transport::Telemetry,
}
impl Worker {
    fn open(model: Model, serial: Option<String>, simulate: bool) -> Result<(Self, String)> {
        let (sender, receiver) = mpsc::channel::<Work>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            let device = if simulate {
                None
            } else {
                match open_camera(model, serial.as_deref()) {
                    Ok(device) => Some(device),
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                }
            };
            let identity = device
                .as_ref()
                .map(|d| d.2.clone())
                .unwrap_or("direct-simulator".into());
            if serial.as_ref().is_some_and(|s| s != &identity) {
                let _ = ready_tx.send(Err(anyhow::anyhow!("camera identity differs")));
                return;
            }
            let telemetry = device
                .as_ref()
                .map(|d| d.0.telemetry())
                .unwrap_or_else(|| std::sync::Arc::new(std::sync::Mutex::new(Some([250, 0]))));
            if ready_tx.send(Ok((identity, telemetry))).is_err() {
                return;
            }
            let (watchdog, deadlines) = mpsc::channel::<Option<Duration>>();
            std::thread::spawn(move || {
                let mut timeout = None;
                loop {
                    let event = if let Some(duration) = timeout {
                        deadlines.recv_timeout(duration)
                    } else {
                        deadlines
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    };
                    match event {
                        Ok(next) => timeout = next,
                        Err(mpsc::RecvTimeoutError::Timeout) => std::process::exit(124),
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            });
            let mut sim_environment = std::collections::HashMap::from([
                (8, 250_i64),
                (15, 0),
                (16, 25),
                (17, 0),
                (21, 0),
                (22, 255),
                (23, 255),
            ]);
            loop {
                let work = match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(work) => work,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some((camera, _, _)) = &device {
                            let _ = watchdog.send(Some(Duration::from_secs(15)));
                            if let Err(e) = camera.service_environment() {
                                crate::diagnostics::log(
                                    "warning",
                                    "environment.failed",
                                    format_args!("{e:#}"),
                                );
                                return;
                            }
                            let _ = watchdog.send(None);
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if let Some((camera, _, _)) = &device
                            && model.cooled()
                        {
                            let _ = watchdog.send(Some(Duration::from_secs(15)));
                            let _ = camera.environment_control(17, Some(0));
                        }
                        return;
                    }
                };
                match work {
                    Work::Environment(control, value, reply) => {
                        let _ = watchdog.send(Some(Duration::from_secs(15)));
                        let result = if let Some((camera, _, _)) = &device {
                            camera.environment_control(control, value)
                        } else {
                            if let Some(value) = value {
                                sim_environment.insert(control, value);
                            }
                            Ok(*sim_environment.get(&control).unwrap_or(&0))
                        };
                        let _ = watchdog.send(None);
                        let _ = reply.send(result);
                    }
                    Work::Capture(
                        settings,
                        gain,
                        bin,
                        timeout,
                        simulated_delay,
                        simulated_cleanup,
                        reply,
                    ) => {
                        // Never free live I/O buffers if a kernel operation becomes stuck.
                        let _ = watchdog.send(Some(timeout));
                        let result = if let Some((camera, info, _)) = &device {
                            // Validated on the command thread, before capture starts.
                            camera
                                .transfer_timeout(settings.transfer_timeout_seconds)
                                .expect("validated transfer timeout");
                            match model {
                                Model::Asi676 => asi676::capture(camera, info, &settings, false),
                                Model::Duo | Model::Asi2600P25 => {
                                    asi2600::capture(camera, info, &settings, gain, bin, false)
                                }
                                Model::Guide => asi220::capture(camera, info, &settings, bin),
                                Model::Asi6200 => {
                                    asi6200::capture(camera, info, &settings, gain, bin, false)
                                }
                            }
                        } else {
                            std::thread::sleep(Duration::from_micros(u64::from(
                                settings.microseconds,
                            )));
                            std::thread::sleep(simulated_delay);
                            let pixels: Vec<_> = (0..settings.width * settings.height)
                                .flat_map(|i| (i as u16).to_le_bytes())
                                .collect();
                            Ok((
                                json!({"width":settings.width,"height":settings.height,"bin":bin,"sdkLoaded":false,
                                "simulated":true,"readRecoveries":0}),
                                pixels,
                            ))
                        };
                        let result = if simulate && simulated_cleanup {
                            crate::completion::finish(
                                result,
                                Err(anyhow::anyhow!("simulated cleanup failure")),
                            )
                        } else {
                            result
                        };
                        if let Some((camera, _, _)) = &device {
                            camera.phase(if result.is_ok() {
                                "image_ready"
                            } else {
                                "failed"
                            });
                        }
                        let _ = watchdog.send(None);
                        if reply.send(result).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        match ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("direct worker exited during open"))?
        {
            Ok((identity, telemetry)) => Ok((
                Self {
                    sender,
                    thread: Some(thread),
                    telemetry,
                },
                identity,
            )),
            Err(e) => {
                let _ = thread.join();
                Err(e)
            }
        }
    }
    fn environment(&self, control: u32, value: Option<i64>) -> Result<i64> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(Work::Environment(control, value, sender))
            .map_err(|_| anyhow::anyhow!("direct worker exited"))?;
        receiver
            .recv_timeout(Duration::from_secs(15))
            .map_err(|_| anyhow::anyhow!("environment worker unavailable"))?
    }
    fn close(mut self) {
        drop(self.sender);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
struct Host {
    worker: Option<Worker>,
    pending: Option<mpsc::Receiver<Result<Frame>>>,
    frame: Option<Result<Frame>>,
    settings: Settings,
    simulate: bool,
    model: Model,
    gain: i32,
    simulated_read_failures: Option<u32>,
    reconnect_required: bool,
    simulated_delay: Duration,
    simulated_cleanup: bool,
}
impl Host {
    fn update(&mut self) {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(frame) => {
                    self.frame = Some(frame);
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.frame = Some(Err(anyhow::anyhow!("direct worker exited")));
                    self.pending = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn command(&mut self, method: &str, params: &Value) -> Result<Frame> {
        self.update();
        let mut pixels = Vec::new();
        let number = |key: &str| -> Result<u32> {
            Ok(u32::try_from(params[key].as_u64().ok_or_else(|| {
                anyhow::anyhow!("missing or invalid {key}")
            })?)?)
        };
        let value = match method {
            "simulation" => {
                ensure!(
                    self.simulate && self.pending.is_none() && self.frame.is_none(),
                    "simulation only, while idle"
                );
                let delay = params["readDelayMs"].as_u64().unwrap_or(0);
                ensure!(delay <= 5000, "invalid simulated read delay");
                self.simulated_delay = Duration::from_millis(delay);
                self.simulated_cleanup = params["cleanupFailure"].as_bool().unwrap_or(false);
                Value::Null
            }
            "simulate-read-failures" => {
                ensure!(
                    self.simulate && self.pending.is_none() && self.frame.is_none(),
                    "simulation only, while idle"
                );
                let failures = number("count")?;
                ensure!(failures <= 6, "invalid simulated read failures");
                self.simulated_read_failures = Some(failures);
                Value::Null
            }
            "list" => {
                let mut found = Vec::new();
                for model in Model::ALL {
                    let count = if self.simulate {
                        1
                    } else {
                        paths(model).map_err(hardware)?.len()
                    };
                    found.extend((0..count).map(|_| model.descriptor()));
                }
                json!(found)
            }
            "open" => {
                ensure!(self.worker.is_none(), "camera is already open");
                let model = Model::ALL
                    .into_iter()
                    .find(|m| params["name"] == m.name())
                    .ok_or_else(|| anyhow::anyhow!("unsupported SDK-less camera model"))?;
                let serial = params["serial"].as_str().map(str::to_owned);
                let (worker, identity) =
                    Worker::open(model, serial, self.simulate).map_err(hardware)?;
                self.model = model;
                self.settings = Settings::default();
                let descriptor = model.descriptor();
                self.settings.width = descriptor["width"].as_u64().unwrap() as u32;
                self.settings.height = descriptor["height"].as_u64().unwrap() as u32;
                self.settings.offset = match model {
                    Model::Asi676 => 10,
                    Model::Duo => 50,
                    Model::Guide => 200,
                    Model::Asi6200 => 50,
                    Model::Asi2600P25 => 1,
                };
                self.gain = 0;
                let mut controls = model.controls();
                if model.cooled() {
                    for cap in &mut controls {
                        let control = cap["type"].as_u64().unwrap() as u32;
                        if [8, 15, 16, 17, 21, 22, 23].contains(&control) {
                            cap["value"] =
                                json!(worker.environment(control, None).map_err(hardware)?);
                        }
                    }
                }
                self.worker = Some(worker);
                self.reconnect_required = false;
                json!({"serial":identity,"info":descriptor,"sdkVersion":format!("SDK-less experimental {}",model.name()),
                    "backend":"direct","controls":controls})
            }
            "get" | "set" => {
                let worker = self
                    .worker
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("camera is not open"))?;
                let control = number("control")?;
                if method == "get"
                    && matches!(control, 8 | 15)
                    && self.model.cooled()
                    && (self.pending.is_some() || self.frame.is_some())
                {
                    // The capture thread already samples these values while servicing
                    // the cooler. Never queue USB work behind a long capture.
                    let sample = worker
                        .telemetry
                        .lock()
                        .unwrap()
                        .ok_or_else(|| anyhow::anyhow!("environment unavailable"))?;
                    return Ok((json!(sample[if control == 8 { 0 } else { 1 }]), pixels));
                }
                ensure!(
                    self.pending.is_none() && self.frame.is_none(),
                    "cannot access controls during capture"
                );
                let caps = self.model.controls();
                let cap = caps
                    .iter()
                    .find(|c| c["type"] == control)
                    .ok_or_else(|| anyhow::anyhow!("unsupported control"))?;
                if method == "set" {
                    ensure!(cap["writable"] == true, "control is read-only");
                    let value = params["value"]
                        .as_i64()
                        .ok_or_else(|| anyhow::anyhow!("invalid control value"))?;
                    ensure!(
                        value >= cap["min"].as_i64().unwrap()
                            && value <= cap["max"].as_i64().unwrap(),
                        "control outside supported range"
                    );
                    match control {
                        0 => {
                            self.gain = value as i32;
                            self.settings.gain = value.max(0) as u32;
                        }
                        1 => self.settings.microseconds = value as u32,
                        5 => self.settings.offset = value as u32,
                        _ => {
                            worker.environment(control, Some(value)).map_err(hardware)?;
                        }
                    }
                    Value::Null
                } else {
                    json!(match control {
                        0 => i64::from(self.gain),
                        1 => i64::from(self.settings.microseconds),
                        5 => i64::from(self.settings.offset),
                        6 => 40,
                        _ => worker.environment(control, None).map_err(hardware)?,
                    })
                }
            }
            "validate" | "start" => {
                let worker = self
                    .worker
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("camera is not open"))?;
                ensure!(
                    self.pending.is_none() && self.frame.is_none() && !self.reconnect_required,
                    "exposure pending or reconnect required after cleanup failure"
                );
                let bin = number("bin")?;
                let mut settings = self.settings.clone();
                settings.width = number("width")?;
                settings.height = number("height")?;
                settings.x = number("x")?;
                settings.y = number("y")?;
                settings.microseconds = number("microseconds")?;
                if !params["readRetries"].is_null() {
                    settings.read_retries = number("readRetries")?;
                }
                if !params["transferTimeoutSeconds"].is_null() {
                    settings.transfer_timeout_seconds =
                        params["transferTimeoutSeconds"]
                            .as_f64()
                            .ok_or_else(|| anyhow::anyhow!("invalid transfer deadline"))?;
                }
                ensure!(
                    settings.transfer_timeout_seconds.is_finite()
                        && settings.transfer_timeout_seconds > 0.0
                        && settings.transfer_timeout_seconds <= 3600.0,
                    "invalid transfer deadline"
                );
                self.model.validate(&settings, self.gain, bin)?;
                if method == "start" {
                    let seconds = params["captureTimeoutSeconds"].as_f64().unwrap_or(
                        f64::from(settings.microseconds) / 1e6
                            + 45.0
                            + settings.transfer_timeout_seconds
                                * f64::from(settings.read_retries + 1),
                    );
                    ensure!(
                        seconds.is_finite() && (0.001..=86400.0).contains(&seconds),
                        "invalid capture deadline"
                    );
                    // Instant faulted readout for the supervisor's policy tests.
                    // This branch is inaccessible without --simulate.
                    if let Some(failures) = self.simulated_read_failures.take() {
                        ensure!(self.simulate, "simulation only");
                        for used in 0..failures.min(settings.read_retries + 1) {
                            crate::diagnostics::read_failure(
                                self.model.name(),
                                "simulated USB read failure",
                                used as usize,
                                settings.read_retries,
                                self.model != Model::Guide,
                            );
                        }
                        if failures > 0 && failures <= settings.read_retries {
                            crate::diagnostics::log(
                                "info",
                                "capture.recovered",
                                format_args!("Simulated frame ready after {failures} read retries"),
                            );
                        }
                        self.frame = Some(if failures > settings.read_retries {
                            Err(anyhow::anyhow!(
                                "simulated read failures exhausted the transfer retry budget"
                            ))
                        } else {
                            Ok((
                                json!({"width":settings.width,"height":settings.height,"simulated":true,
                                "readRecoveries":failures}),
                                vec![0; (settings.width * settings.height * 2) as usize],
                            ))
                        });
                        return Ok((Value::Null, Vec::new()));
                    }
                    let (sender, receiver) = mpsc::sync_channel(1);
                    worker
                        .sender
                        .send(Work::Capture(
                            settings,
                            self.gain,
                            bin,
                            Duration::from_secs_f64(seconds),
                            self.simulated_delay,
                            self.simulated_cleanup,
                            sender,
                        ))
                        .map_err(|_| hardware(anyhow::anyhow!("direct worker exited")))?;
                    self.pending = Some(receiver);
                }
                Value::Null
            }
            "status" => {
                ensure!(self.worker.is_some(), "camera is not open");
                if let Some(Err(error)) = &self.frame {
                    if let Some(failure) = error.downcast_ref::<crate::transfer::Failure>() {
                        return Err(hardware(
                            anyhow::Error::new(failure.clone()).context(format!("{error:#}")),
                        ));
                    }
                    return Err(hardware(anyhow::anyhow!("{error:#}")));
                }
                json!(if self.pending.is_some() {
                    1
                } else if self.frame.is_some() {
                    2
                } else {
                    0
                })
            }
            "download" => {
                let frame = self
                    .frame
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("no completed frame"))?
                    .map_err(hardware)?;
                pixels = frame.1;
                self.reconnect_required = frame.0.get("cleanupError").is_some();
                frame.0
            }
            "stop" | "close" => {
                ensure!(
                    self.pending.is_none(),
                    "active capture must be aborted by terminating the isolated host"
                );
                self.frame = None;
                if method == "close"
                    && let Some(worker) = self.worker.take()
                {
                    // Report failed cooler shutdown to the supervisor so its bounded
                    // reconnect-for-cleanup path can run. Channel teardown is still
                    // unconditional, even if the explicit off command fails.
                    let cooling = if self.model.cooled() {
                        worker.environment(17, Some(0)).map(|_| ())
                    } else {
                        Ok(())
                    };
                    worker.close();
                    cooling.map_err(hardware)?;
                }
                Value::Null
            }
            _ => bail!("unknown direct method {method}"),
        };
        Ok((value, pixels))
    }
}

pub fn run(simulate: bool) -> Result<()> {
    transport::require_sdk_absent()?;
    let mut host = Host {
        simulate,
        ..Host::default()
    };
    let (mut input, mut output) = (std::io::stdin().lock(), std::io::stdout().lock());
    loop {
        let mut length = [0_u8; 4];
        match input.read_exact(&mut length) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        }
        let size = u32::from_le_bytes(length) as usize;
        ensure!((1..=65536).contains(&size), "invalid request length");
        let mut bytes = vec![0; size];
        input.read_exact(&mut bytes)?;
        let request: Value = serde_json::from_slice(&bytes)?;
        ensure!(request["version"] == 1, "unsupported protocol");
        let began = Instant::now();
        let (reply, pixels) = match host
            .command(request["method"].as_str().unwrap_or(""), &request["params"])
        {
            Ok((result, pixels)) => (
                json!({"version":1,"id":request["id"],"ok":true,
                "result":result,"binaryLength":pixels.len()}),
                pixels,
            ),
            Err(error) => {
                crate::diagnostics::log(
                    "warning",
                    "command.failed",
                    format_args!("{} request {}: {error:#}", request["method"], request["id"]),
                );
                (
                    json!({"version":1,"id":request["id"],"ok":false,"error":format!("{error:#}"),
                "sdkCode":if error.is::<HardwareFailure>() { None } else {Some(8)},
                "sdkOperation":"direct","transportFailure":crate::transfer::Failure::details(&error),"binaryLength":0}),
                    Vec::new(),
                )
            }
        };
        transport::require_sdk_absent()?;
        if request["method"] == "download" {
            crate::diagnostics::log(
                "debug",
                "frame.delivered",
                format_args!(
                    "{} bytes (IPC preparation {} ms)",
                    pixels.len(),
                    began.elapsed().as_millis()
                ),
            );
        }
        let json = serde_json::to_vec(&reply)?;
        output.write_all(&(json.len() as u32).to_le_bytes())?;
        output.write_all(&json)?;
        output.write_all(&pixels)?;
        output.flush()?;
    }
}
