//! Version-1 plugin protocol over inherited pipes for the verified camera interfaces.
//! A dedicated worker owns the exclusive driver handle for the entire connection.
use crate::asi::direct::{
    asi220, asi585, asi662, asi676, asi2600, asi6200, bayer_video, settings::Settings, transport,
};
use anyhow::{Result, bail, ensure};
use regain_core::white_balance::{Geometry, Settings as WhiteBalanceSettings, WhiteBalance};
use serde_json::{Value, json};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

type Frame = (Value, Vec<u8>);
enum Work {
    Capture(
        Settings,
        bool,
        i32,
        u32,
        Duration,
        Duration,
        bool,
        u32,
        mpsc::SyncSender<Result<Frame>>,
    ),
    Environment(u32, Option<i64>, mpsc::SyncSender<Result<i64>>),
    StopVideo(mpsc::SyncSender<Result<()>>),
}
#[derive(Clone, Copy, Default, PartialEq)]
enum Model {
    #[default]
    Asi676,
    Asi662,
    Asi585,
    Duo,
    Guide,
    Asi6200,
    Asi2600P25,
}
impl Model {
    fn video_profile(self) -> Option<&'static super::bayer::Profile> {
        match self {
            Self::Asi662 => Some(&asi662::PROFILE),
            Self::Asi585 => Some(&asi585::PROFILE),
            Self::Asi676 => Some(&asi676::PROFILE),
            _ => None,
        }
    }
    const ALL: [Self; 7] = [
        Self::Asi676,
        Self::Asi662,
        Self::Asi585,
        Self::Duo,
        Self::Guide,
        Self::Asi6200,
        Self::Asi2600P25,
    ];
    fn cooled(self) -> bool {
        matches!(
            self,
            Self::Duo | Self::Asi6200 | Self::Asi2600P25 | Self::Asi585
        )
    }
    fn name(self) -> &'static str {
        match self {
            Self::Asi676 => "ZWO ASI676MC",
            Self::Asi662 => "ZWO ASI662MC",
            Self::Asi585 => "ZWO ASI585MM Pro",
            Self::Duo => "ZWO ASI2600MM Duo",
            Self::Guide => "ZWO ASI220MM Mini",
            Self::Asi6200 => "ZWO ASI6200MM Pro",
            Self::Asi2600P25 => "ZWO ASI2600MM Pro",
        }
    }
    fn pid(self) -> u32 {
        match self {
            Self::Asi676 => 0x676d,
            Self::Asi662 => 0x662b,
            Self::Asi585 => 0x585e,
            Self::Duo => 0x2601,
            Self::Guide => 0x2209,
            Self::Asi6200 => 0x620b,
            Self::Asi2600P25 => 0x260e,
        }
    }
    fn descriptor(self) -> Value {
        let (width, height, pixel, bits, bins, alignment) = match self {
            Self::Asi676 => (3552, 3552, 2.0, 12, vec![1], 2),
            Self::Asi662 => (1920, 1080, 2.9, 12, vec![1], 8),
            Self::Asi585 => (3840, 2160, 2.9, 12, vec![1, 2, 3, 4], 4),
            Self::Duo | Self::Asi2600P25 => (6248, 4176, 3.76, 16, vec![1, 2, 3, 4], 16),
            Self::Guide => (1920, 1080, 4.0, 12, vec![1, 2], 2),
            Self::Asi6200 => (9576, 6388, 3.76, 16, vec![1, 2, 3, 4], 16),
        };
        json!({"id":self.pid(),"name":self.name(),"width":width,"height":height,"color":matches!(self, Self::Asi676 | Self::Asi662),"bayer":0,
            "pixelSize":pixel,"bitDepth":bits,"cooled":self.cooled(),"shutter":false,"bins":bins,"formats":[2],
            "minimumWidth":64,"minimumHeight":64,"originAlignment":alignment,
            "retainedFrameReads":self != Self::Guide,
            "readRetryOverheadSeconds":if cfg!(windows) && self == Self::Asi2600P25 {15} else {0}})
    }
    fn controls(self, auxiliary: bool) -> Vec<Value> {
        let (gain_min, gain_max, offset_min, offset_max, offset_default, exp_max) = match self {
            Self::Asi676 => (0, 600, 0, 200, 10, super::settings::MAX_EXPOSURE_US as i32),
            Self::Asi662 => (0, 600, 0, 300, 15, super::settings::MAX_EXPOSURE_US as i32),
            Self::Asi585 => (0, 600, 0, 300, 3, super::settings::MAX_EXPOSURE_US as i32),
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
                if kind == 21 && self == Self::Asi585 {
                    continue;
                }
                caps.push(
                    json!({"type":kind,"min":min,"max":max,"value":value,"writable":writable}),
                );
            }
        }
        if auxiliary {
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
            Self::Asi662 => {
                ensure!(bin == 1, "ASI662MC direct capture supports bin 1 only");
                asi662::PROFILE.validate(settings)
            }
            Self::Asi585 => asi585::raw_settings(settings, bin).map(|_| ()),
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

pub(super) fn usb_target(name: &str, serial: &str, locator: Option<&str>) -> Result<()> {
    ensure!(
        serial.len() == 16
            && serial.bytes().all(|c| c.is_ascii_hexdigit())
            && serial != "0000000000000000",
        "USB recovery requires a saved camera serial"
    );
    let model = Model::ALL
        .into_iter()
        .find(|m| m.name() == name)
        .ok_or_else(|| anyhow::anyhow!("USB recovery is unavailable for this camera model"))?;
    let mut targets = Vec::new();
    let candidates = selected_paths(paths(model)?, locator, |p| p.locator())?;
    ensure!(
        candidates.len() == 1,
        "USB recovery requires an unambiguous interface; refusing serial sweep"
    );
    for path in candidates {
        // Busy devices are not seized. Only the selected model's serial query is sent.
        let Ok(camera) = transport::Camera::open(&path) else {
            continue;
        };
        let Ok(bytes) = camera.vendor(0xc8, 0, 0, 8) else {
            continue;
        };
        let found: String = bytes.iter().map(|v| format!("{v:02x}")).collect();
        if found.eq_ignore_ascii_case(serial) {
            targets.push(path.recovery_target(model.pid() as u16, found)?);
        }
    }
    ensure!(
        targets.len() == 1,
        "Cannot uniquely bind USB recovery to this camera serial; close other camera owners"
    );
    println!("{}", targets[0].encode()?);
    Ok(())
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

fn open_camera(
    model: Model,
    serial: Option<&str>,
    locator: Option<&str>,
) -> Result<(transport::Camera, Value, String)> {
    let paths = selected_paths(paths(model)?, locator, |p| p.locator())?;
    ensure!(!paths.is_empty(), "selected direct camera is not attached");
    ensure!(
        serial.is_some() || paths.len() == 1,
        "multiple cameras of this model require a serial number"
    );
    let (camera, mut info, found) = find_accessible(paths, |path| {
        let camera = transport::Camera::open(&path)?;
        let info = camera.probe()?;
        let link = super::link::validate(&info, model.pid())?;
        // SDK 1.41 ASIGetSerialNumber uses vendor IN C8, value/index 0, eight bytes.
        let bytes = camera.vendor(0xc8, 0, 0, 8)?;
        ensure!(
            bytes.iter().any(|&v| v != 0),
            "camera serial is unavailable"
        );
        let found: String = bytes.iter().map(|v| format!("{v:02x}")).collect();
        if serial.is_none_or(|s| s == found) {
            super::diagnostics::log(
                "info",
                "camera.transport",
                format_args!("{}: {}", model.name(), link.label()),
            );
            return Ok(Some((camera, info, found)));
        }
        Ok(None)
    })?;
    let auxiliary = match model {
        Model::Asi6200 => asi6200::Revision::detect(&camera)? == asi6200::Revision::P25,
        Model::Asi2600P25 => true,
        _ => false,
    };
    info["auxiliaryControls"] = json!(auxiliary);
    if model.cooled() {
        camera.enable_environment(
            auxiliary,
            model != Model::Asi585,
            if model == Model::Asi585 {
                super::environment::CoolerOutput::Dac
            } else {
                super::environment::CoolerOutput::Fpga
            },
        )?;
    }
    Ok((camera, info, found))
}

fn selected_paths<T>(
    paths: Vec<T>,
    locator: Option<&str>,
    key: impl Fn(&T) -> String,
) -> Result<Vec<T>> {
    if let Some(locator) = locator {
        ensure!(!locator.is_empty(), "empty camera locator");
        let matches: Vec<_> = paths.into_iter().filter(|p| key(p) == locator).collect();
        ensure!(
            matches.len() == 1,
            "selected camera interface missing or ambiguous; no fallback discovery"
        );
        Ok(matches)
    } else {
        Ok(paths)
    }
}

#[test]
fn targeted_paths_never_probe_a_different_camera_or_fall_back() {
    let paths = vec!["other", "selected", "busy"];
    let selected = selected_paths(paths.clone(), Some("selected"), |p| p.to_string()).unwrap();
    let mut opened = Vec::new();
    let _: Result<()> = find_accessible(selected, |p| {
        opened.push(p);
        bail!("selected device busy")
    });
    assert_eq!(opened, ["selected"]);
    assert!(selected_paths(paths, Some("missing"), |p| p.to_string()).is_err());
    assert!(selected_paths(vec!["same", "same"], Some("same"), |p| p.to_string()).is_err());
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
                crate::asi::direct::diagnostics::log(
                    "warning",
                    "camera.unavailable",
                    format_args!("Skipping camera candidate: {error:#}"),
                );
                last_error = Some(error);
            }
        }
    }
    if let Some(error) = last_error {
        return Err(error
            .context("could not open or identify an eligible camera; see the USB failure below"));
    }
    bail!("selected camera serial is not attached")
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    #[test]
    fn serial_discovery_and_blank_selection_do_not_require_hardware() {
        let mut host = Host {
            simulate: true,
            ..Host::default()
        };
        let list = host.command("list", &json!({"serials":true})).unwrap().0;
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .all(|c| c["serial"] == "direct-simulator")
        );
        for serial in [
            Value::Null,
            json!(""),
            json!("  "),
            json!(" DIRECT-SIMULATOR "),
        ] {
            host.command("open", &json!({"name":"ZWO ASI662MC","serial":serial}))
                .unwrap();
            assert!(host.command("list", &json!({"serials":true})).is_err());
            host.command("close", &Value::Null).unwrap();
        }
        assert!(
            host.command("open", &json!({"name":"ZWO ASI662MC","serial":"wrong"}))
                .is_err()
        );
        let error = find_accessible([0], |_| -> Result<Option<()>> {
            bail!("synthetic USB busy")
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("synthetic USB busy"));
        assert!(!error.to_string().contains("serial"));
    }
    #[test]
    fn original_6200_does_not_advertise_or_accept_p25_auxiliary_controls() {
        let caps = Model::Asi6200.controls(false);
        assert!(caps.iter().all(|c| c["type"] != 22 && c["type"] != 23));
        let mut host = Host {
            simulate: true,
            ..Host::default()
        };
        host.command("open", &json!({"name":"ZWO ASI6200MM Pro"}))
            .unwrap();
        host.worker.as_mut().unwrap().auxiliary = false;
        for control in [22, 23] {
            for method in ["get", "set"] {
                let error = host
                    .command(method, &json!({"control":control,"value":128}))
                    .unwrap_err();
                assert_eq!(error.to_string(), "unsupported control");
                assert!(!error.is::<HardwareFailure>());
            }
        }
        host.command("close", &Value::Null).unwrap();
    }
    #[test]
    fn hardware_classification_preserves_transport_details() {
        let error =
            hardware(crate::asi::direct::transfer::Failure::new("timeout", 1024, 0, true).into());
        assert!(error.is::<HardwareFailure>());
        assert_eq!(
            crate::asi::direct::transfer::Failure::details(&error)["category"],
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
        host.frame = Some(Err(crate::asi::direct::transfer::Failure::new(
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
                crate::asi::direct::transfer::Failure::details(&error)["receivedBytes"],
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
    cooling: super::environment::CoolingQueue,
    auxiliary: bool,
    locator: String,
    cancel_video: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl Worker {
    fn open(
        model: Model,
        serial: Option<String>,
        locator: Option<String>,
        simulate: bool,
    ) -> Result<(Self, String)> {
        let (sender, receiver) = mpsc::channel::<Work>();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let cancel_video = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled = cancel_video.clone();
        let thread = std::thread::spawn(move || {
            let device = if simulate {
                if locator
                    .as_deref()
                    .is_some_and(|s| s != "simulated-interface")
                {
                    let _ =
                        ready_tx.send(Err(anyhow::anyhow!("simulated camera interface changed")));
                    return;
                }
                None
            } else {
                match open_camera(model, serial.as_deref(), locator.as_deref()) {
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
            let telemetry = device.as_ref().map(|d| d.0.telemetry()).unwrap_or_else(|| {
                std::sync::Arc::new(std::sync::Mutex::new(Some([250, 0, 25, 0])))
            });
            let cooling = device
                .as_ref()
                .map(|d| d.0.cooling_queue())
                .unwrap_or_default();
            let auxiliary = device
                .as_ref()
                .map(|d| d.1["auxiliaryControls"] == true)
                .unwrap_or(matches!(model, Model::Asi6200 | Model::Asi2600P25));
            let locator = device
                .as_ref()
                .map(|d| d.0.locator())
                .unwrap_or_else(|| "simulated-interface".into());
            if ready_tx
                .send(Ok((
                    identity,
                    telemetry.clone(),
                    auxiliary,
                    locator,
                    cooling.clone(),
                )))
                .is_err()
            {
                return;
            }
            let _cooling_owner = cooling.owner();
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
            let sim_environment = std::cell::RefCell::new(std::collections::HashMap::from([
                (8, 250_i64),
                (15, 0),
                (16, 25),
                (17, 0),
                (21, 0),
                (22, 255),
                (23, 255),
            ]));
            let mut video: Option<bayer_video::Video> = None;
            let mut simulated_video: Option<Settings> = None;
            let mut simulated_sequence = 0_u64;
            let mut video_pacer = bayer_video::Pacer::default();
            loop {
                let work = match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(work) => work,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if let Some((camera, _, _)) = &device {
                            let _ = watchdog.send(Some(Duration::from_secs(15)));
                            if let Err(e) = camera.service_environment() {
                                crate::asi::direct::diagnostics::log(
                                    "warning",
                                    "environment.failed",
                                    format_args!("{e:#}"),
                                );
                                return;
                            }
                            let _ = watchdog.send(None);
                        } else {
                            service_simulated_cooling(&cooling, &sim_environment, &telemetry);
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if let Some(mut video) = video.take()
                            && let Some((camera, _, _)) = &device
                        {
                            let _ = watchdog.send(Some(Duration::from_secs(15)));
                            let _ = video.stop(camera);
                        }
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
                    Work::StopVideo(reply) => {
                        let _ = watchdog.send(Some(Duration::from_secs(15)));
                        let result = if let Some(mut video) = video.take()
                            && let Some((camera, _, _)) = &device
                        {
                            video.stop(camera)
                        } else {
                            Ok(())
                        };
                        simulated_video = None;
                        video_pacer = bayer_video::Pacer::default();
                        let _ = watchdog.send(None);
                        let _ = reply.send(result);
                    }
                    Work::Environment(control, value, reply) => {
                        let _ = watchdog.send(Some(Duration::from_secs(15)));
                        let result = if let Some((camera, _, _)) = &device {
                            camera.environment_control(control, value)
                        } else {
                            if let Some(value) = value {
                                sim_environment.borrow_mut().insert(control, value);
                            }
                            Ok(*sim_environment.borrow().get(&control).unwrap_or(&0))
                        };
                        let _ = watchdog.send(None);
                        let _ = reply.send(result);
                    }
                    Work::Capture(
                        settings,
                        video_mode,
                        gain,
                        bin,
                        timeout,
                        simulated_delay,
                        simulated_cleanup,
                        mut simulated_framing_failures,
                        reply,
                    ) => {
                        // Never free live I/O buffers if a kernel operation becomes stuck.
                        let _ = watchdog.send(Some(timeout));
                        let result = (|| -> Result<Frame> {
                            if video_mode && !settings.continuous_drain {
                                video_pacer.wait_servicing(
                                    settings.video_max_fps,
                                    &cancelled,
                                    || {
                                        if let Some((camera, _, _)) = &device {
                                            camera.service_environment()?;
                                        }
                                        Ok(())
                                    },
                                )?;
                            }
                            if let Some((camera, info, _)) = &device {
                                // Validated on the command thread, before capture starts.
                                camera
                                    .transfer_timeout(settings.transfer_timeout_seconds)
                                    .expect("validated transfer timeout");
                                if video_mode {
                                    let requested = settings.clone();
                                    let settings = if model == Model::Asi585 {
                                        asi585::raw_settings(&settings, bin)?
                                    } else {
                                        settings
                                    };
                                    if let Some(session) = video.as_mut()
                                        && settings.continuous_drain
                                        && session.can_update(&settings)
                                    {
                                        session.update(camera, info, &settings)?;
                                    }
                                    if video.as_ref().is_none_or(|v| !v.matches(&settings)) {
                                        if let Some(mut old) = video.take() {
                                            old.stop(camera)?;
                                        }
                                        ensure!(
                                            !cancelled.load(std::sync::atomic::Ordering::Relaxed),
                                            "video read cancelled"
                                        );
                                        video = Some(bayer_video::Video::start(
                                            camera,
                                            info,
                                            settings.clone(),
                                            model.video_profile().expect("validated video model"),
                                        )?);
                                    }
                                    let session = video.as_mut().unwrap();
                                    session.set_max_fps(settings.video_max_fps);
                                    let frame = session.next(camera, info, &cancelled)?;
                                    if model == Model::Asi585 {
                                        asi585::finish(frame, &requested, bin)
                                    } else {
                                        Ok(frame)
                                    }
                                } else {
                                    if let Some(mut old) = video.take() {
                                        old.stop(camera)?;
                                    }
                                    match model {
                                        Model::Asi676 => {
                                            asi676::capture(camera, info, &settings, false)
                                        }
                                        Model::Asi585 => {
                                            asi585::capture(camera, info, &settings, bin, false)
                                        }
                                        Model::Asi662 => {
                                            asi662::capture(camera, info, &settings, false)
                                        }
                                        Model::Duo | Model::Asi2600P25 => asi2600::capture(
                                            camera, info, &settings, gain, bin, false,
                                        ),
                                        Model::Guide => {
                                            asi220::capture(camera, info, &settings, bin)
                                        }
                                        Model::Asi6200 => asi6200::capture(
                                            camera, info, &settings, gain, bin, false,
                                        ),
                                    }
                                }
                            } else {
                                let mut framing_recoveries = 0;
                                if video_mode {
                                    // FPS changes pace the existing stream; they do
                                    // not reconfigure the sensor on real hardware.
                                    if let Some(prior) = &mut simulated_video {
                                        prior.video_max_fps = settings.video_max_fps;
                                    }
                                    if simulated_video.as_ref() != Some(&settings) {
                                        simulated_sequence = 0;
                                        simulated_video = Some(settings.clone());
                                    }
                                    let (_, recovered) = bayer_video::recover_frame_once(
                                        &mut simulated_framing_failures,
                                        &cancelled,
                                        |remaining| {
                                            bayer_video::wait_until_servicing(
                                                Instant::now(),
                                                Duration::from_micros(u64::from(
                                                    settings.microseconds,
                                                )),
                                                &cancelled,
                                                || {
                                                    service_simulated_cooling(
                                                        &cooling,
                                                        &sim_environment,
                                                        &telemetry,
                                                    );
                                                    Ok(())
                                                },
                                            )?;
                                            if *remaining > 0 {
                                                *remaining -= 1;
                                                return Err(super::protocol::FrameBoundaryError(
                                                    "invalid frame boundary markers",
                                                )
                                                .into());
                                            }
                                            Ok(())
                                        },
                                        |_| {
                                            if settings.continuous_drain {
                                                return Ok(());
                                            }
                                            bayer_video::wait_until_servicing(
                                                Instant::now(),
                                                bayer_video::frame_interval(
                                                    settings.video_max_fps,
                                                )?,
                                                &cancelled,
                                                || {
                                                    service_simulated_cooling(
                                                        &cooling,
                                                        &sim_environment,
                                                        &telemetry,
                                                    );
                                                    Ok(())
                                                },
                                            )
                                        },
                                    )?;
                                    framing_recoveries = recovered;
                                    simulated_sequence += 1;
                                } else {
                                    simulated_video = None;
                                    bayer_video::wait_until_servicing(
                                        Instant::now(),
                                        Duration::from_micros(u64::from(settings.microseconds)),
                                        &cancelled,
                                        || {
                                            service_simulated_cooling(
                                                &cooling,
                                                &sim_environment,
                                                &telemetry,
                                            );
                                            Ok(())
                                        },
                                    )?;
                                }
                                bayer_video::wait_until_servicing(
                                    Instant::now(),
                                    simulated_delay,
                                    &cancelled,
                                    || {
                                        service_simulated_cooling(
                                            &cooling,
                                            &sim_environment,
                                            &telemetry,
                                        );
                                        Ok(())
                                    },
                                )?;
                                let pixels: Vec<_> = (0..settings.width * settings.height)
                                    .flat_map(|i| (i as u16).to_le_bytes())
                                    .collect();
                                Ok((
                                    json!({"width":settings.width,"height":settings.height,"bin":bin,"sdkLoaded":false,
                                "simulated":true,"readRecoveries":0,"mode":if video_mode { "video" } else { "still" },
                                "deliveredFrames":simulated_sequence,"framingRecoveries":framing_recoveries}),
                                    pixels,
                                ))
                            }
                        })();
                        if video_mode && result.is_ok() {
                            video_pacer.completed();
                        }
                        let result = if simulate && simulated_cleanup {
                            crate::asi::direct::completion::finish(
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
            Ok((identity, telemetry, auxiliary, locator, cooling)) => Ok((
                Self {
                    sender,
                    thread: Some(thread),
                    telemetry,
                    cooling,
                    auxiliary,
                    locator,
                    cancel_video,
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
        if matches!(control, 16 | 17)
            && let Some(value) = value
        {
            return self
                .cooling
                .call(control, value, Duration::from_secs(15))
                .map_err(Into::into);
        }
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(Work::Environment(control, value, sender))
            .map_err(|_| anyhow::anyhow!("direct worker exited"))?;
        receiver
            .recv_timeout(Duration::from_secs(15))
            .map_err(|_| anyhow::anyhow!("environment worker unavailable"))?
    }
    fn close(mut self) {
        self.cancel_video
            .store(true, std::sync::atomic::Ordering::Relaxed);
        drop(self.sender);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
    fn stop_video(&self) -> Result<()> {
        self.cancel_video
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        self.sender
            .send(Work::StopVideo(sender))
            .map_err(|_| anyhow::anyhow!("direct worker exited"))?;
        receiver
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| anyhow::anyhow!("video stop deadline expired; terminate isolated host"))?
    }
}

fn service_simulated_cooling(
    cooling: &super::environment::CoolingQueue,
    environment: &std::cell::RefCell<std::collections::HashMap<u32, i64>>,
    telemetry: &transport::Telemetry,
) {
    cooling.service(|control, value| {
        environment.borrow_mut().insert(control, value);
        let environment = environment.borrow();
        *telemetry.lock().unwrap() = Some([
            environment[&8],
            environment[&15],
            environment[&16],
            environment[&17],
        ]);
        Ok(value)
    });
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
    simulated_framing_failures: u32,
    white_balance: Option<WhiteBalance>,
    white_balance_frame: Option<Geometry>,
    video_active: bool,
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
                let failures = params["framingFailures"].as_u64().unwrap_or(0);
                ensure!(failures <= 2, "invalid simulated framing failures");
                self.simulated_framing_failures = failures as u32;
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
                ensure!(
                    self.worker.is_none(),
                    "Direct USB discovery is unavailable while a camera is open; use the cached camera list"
                );
                let mut found = Vec::new();
                // One OS metadata enumeration, not one pass per supported model.
                let devices = if self.simulate {
                    Vec::new()
                } else {
                    transport::enumerate().map_err(hardware)?
                };
                for model in Model::ALL {
                    if self.simulate {
                        let mut info = model.descriptor();
                        if params["serials"] == true {
                            info["serial"] = json!("direct-simulator");
                        }
                        found.push(info);
                        continue;
                    }
                    for path in devices
                        .iter()
                        .filter(|p| p.matches(0x03c3, model.pid() as u16))
                    {
                        let mut info = model.descriptor();
                        info["locator"] = json!(path.locator());
                        if params["serials"] == true {
                            // Identity probing is explicitly opt-in, before ownership.
                            // Drop each handle before inspecting another; never seize a busy device.
                            let serial = (|| -> Result<String> {
                                let camera = transport::Camera::open(path)?;
                                let bytes = camera.vendor(0xc8, 0, 0, 8)?;
                                ensure!(
                                    bytes.len() == 8 && bytes.iter().any(|&b| b != 0),
                                    "camera serial is unavailable"
                                );
                                Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
                            })();
                            match serial {
                                Ok(serial) => info["serial"] = json!(serial),
                                Err(error) => info["discoveryError"] = json!(format!("{error:#}")),
                            }
                        }
                        found.push(info);
                    }
                }
                json!(found)
            }
            "open" => {
                ensure!(self.worker.is_none(), "camera is already open");
                let model = Model::ALL
                    .into_iter()
                    .find(|m| params["name"] == m.name())
                    .ok_or_else(|| anyhow::anyhow!("unsupported SDK-less camera model"))?;
                let serial = crate::asi::normalized_serial(params["serial"].as_str());
                let locator = params
                    .get("locator")
                    .map(|v| {
                        v.as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .ok_or_else(|| anyhow::anyhow!("invalid camera locator"))
                    })
                    .transpose()?;
                let (worker, identity) =
                    Worker::open(model, serial, locator, self.simulate).map_err(hardware)?;
                self.model = model;
                self.settings = Settings::default();
                let mut descriptor = model.descriptor();
                descriptor["locator"] = json!(worker.locator);
                self.settings.width = descriptor["width"].as_u64().unwrap() as u32;
                self.settings.height = descriptor["height"].as_u64().unwrap() as u32;
                self.settings.offset = match model {
                    Model::Asi676 => 10,
                    Model::Asi662 => 15,
                    Model::Asi585 => 3,
                    Model::Duo => 50,
                    Model::Guide => 200,
                    Model::Asi6200 => 50,
                    Model::Asi2600P25 => 1,
                };
                self.gain = 0;
                let mut controls = model.controls(worker.auxiliary);
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
                self.white_balance = None;
                self.white_balance_frame = None;
                json!({"serial":identity,"info":descriptor,"sdkVersion":format!("SDK-less experimental {}",model.name()),
                    "backend":"direct","controls":controls,
                    "captureModes":if model.video_profile().is_some() { json!(["still","video"]) } else { json!(["still"]) },
                    "videoMaxExposureMicroseconds":if model.video_profile().is_some() { json!(30_000_000) } else { Value::Null },
                    "videoFrameRecoveryAttempts":if model.video_profile().is_some() { json!(1) } else { json!(0) },
                    "whiteBalance":WhiteBalance::capabilities(descriptor["color"] == true)})
            }
            "white-balance" => {
                ensure!(self.worker.is_some(), "camera is not open");
                if !params.is_null() && params.as_object().is_none_or(|o| !o.is_empty()) {
                    ensure!(
                        self.pending.is_none() && self.frame.is_none(),
                        "cannot change white balance during capture"
                    );
                    ensure!(
                        self.model.descriptor()["color"] == true,
                        "managed white balance unavailable"
                    );
                    let settings: WhiteBalanceSettings = serde_json::from_value(params.clone())?;
                    let mut next = self.white_balance.clone().unwrap_or_default();
                    next.configure(settings)?;
                    self.white_balance = Some(next);
                }
                json!({"capabilities":WhiteBalance::capabilities(self.model.descriptor()["color"] == true),
                    "managed":self.white_balance.is_some(),
                    "settings":self.white_balance.as_ref().map(WhiteBalance::settings)})
            }
            "get" | "set" => {
                let worker = self
                    .worker
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("camera is not open"))?;
                let control = number("control")?;
                if method == "get"
                    && matches!(control, 8 | 15 | 16 | 17)
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
                    let index = match control {
                        8 => 0,
                        15 => 1,
                        16 => 2,
                        17 => 3,
                        _ => unreachable!(),
                    };
                    return Ok((json!(sample[index]), pixels));
                }
                ensure!(
                    (self.pending.is_none() && self.frame.is_none())
                        || (self.model.cooled() && matches!(control, 16 | 17)),
                    "cannot access controls during capture"
                );
                let caps = self.model.controls(worker.auxiliary);
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
                            if let Err(error) = worker.environment(control, Some(value)) {
                                if matches!(
                                    error.downcast_ref::<super::environment::CoolingError>(),
                                    Some(super::environment::CoolingError::Uncertain(_))
                                ) {
                                    self.reconnect_required = true;
                                }
                                return Err(hardware(error));
                            }
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
                    (method == "validate" || (self.pending.is_none() && self.frame.is_none()))
                        && !self.reconnect_required,
                    "exposure pending or reconnect required after cleanup failure"
                );
                let bin = number("bin")?;
                let mut settings = self.settings.clone();
                settings.continuous_drain = params["continuousDrain"] == true;
                settings.width = number("width")?;
                settings.height = number("height")?;
                settings.x = number("x")?;
                settings.y = number("y")?;
                settings.microseconds = number("microseconds")?;
                if params.get("gain").is_some() {
                    settings.gain = number("gain")?;
                }
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
                let gain = i32::try_from(settings.gain)?;
                self.model.validate(&settings, gain, bin)?;
                let mode = params.get("mode").map_or(Some("still"), Value::as_str);
                ensure!(
                    matches!(mode, Some("still" | "video")),
                    "unknown capture mode"
                );
                let video_mode = mode == Some("video");
                if video_mode {
                    ensure!(
                        self.model.video_profile().is_some(),
                        "video is only available for ASI585MM Pro/ASI662MC/ASI676MC"
                    );
                    ensure!(
                        params["dark"] != true,
                        "video dark-frame semantics are unavailable; cap the camera explicitly"
                    );
                    settings.video_max_fps = params.get("maxFps").map_or(Ok(1.0), |v| {
                        v.as_f64()
                            .ok_or_else(|| anyhow::anyhow!("invalid video maxFps"))
                    })?;
                    bayer_video::validate(
                        &if self.model == Model::Asi585 {
                            asi585::raw_settings(&settings, bin)?
                        } else {
                            settings.clone()
                        },
                        self.model.video_profile().unwrap(),
                    )?;
                    // Live video is never retried as the same retained image.
                    ensure!(
                        params["readRetries"].as_u64().is_none_or(|v| v == 0),
                        "video does not support retained read retries"
                    );
                    settings.read_retries = 0;
                }
                if self.white_balance.is_some() {
                    WhiteBalance::validate_geometry(
                        self.model.descriptor()["color"] == true,
                        Some(0),
                        bin,
                    )?;
                }
                if method == "start" {
                    self.white_balance_frame = Some(Geometry {
                        width: settings.width as usize,
                        height: settings.height as usize,
                        x: settings.x,
                        y: settings.y,
                        bayer: 0,
                        dark: params["dark"].as_bool().unwrap_or(false),
                    });
                    let seconds = params["captureTimeoutSeconds"].as_f64().unwrap_or(
                        f64::from(settings.microseconds) / 1e6 * if video_mode { 2.0 } else { 1.0 }
                            + if video_mode {
                                2.0 / settings.video_max_fps
                            } else {
                                0.0
                            }
                            + 45.0
                            + (settings.transfer_timeout_seconds + 15.0)
                                * if video_mode {
                                    2.0
                                } else {
                                    f64::from(settings.read_retries + 1)
                                },
                    );
                    ensure!(
                        seconds.is_finite() && (0.001..=86400.0).contains(&seconds),
                        "invalid capture deadline"
                    );
                    if self.video_active && !video_mode {
                        worker.stop_video().map_err(hardware)?;
                    }
                    self.video_active = video_mode;
                    worker
                        .cancel_video
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    // Instant faulted readout for the supervisor's policy tests.
                    // This branch is inaccessible without --simulate.
                    if let Some(failures) = self.simulated_read_failures.take() {
                        ensure!(self.simulate, "simulation only");
                        for used in 0..failures.min(settings.read_retries + 1) {
                            crate::asi::direct::diagnostics::read_failure(
                                self.model.name(),
                                "simulated USB read failure",
                                used as usize,
                                settings.read_retries,
                                self.model != Model::Guide,
                            );
                        }
                        if failures > 0 && failures <= settings.read_retries {
                            crate::asi::direct::diagnostics::log(
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
                            video_mode,
                            gain,
                            bin,
                            Duration::from_secs_f64(seconds),
                            self.simulated_delay,
                            self.simulated_cleanup,
                            self.simulated_framing_failures,
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
                    if let Some(failure) =
                        error.downcast_ref::<crate::asi::direct::transfer::Failure>()
                    {
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
                let mut metadata = frame.0;
                if let Some(wb) = &mut self.white_balance {
                    metadata["whiteBalance"] = wb.process(
                        &mut pixels,
                        self.white_balance_frame
                            .ok_or_else(|| anyhow::anyhow!("missing white balance geometry"))?,
                    )?;
                }
                metadata
            }
            "stop" | "close" => {
                if self.video_active {
                    let stopped = self
                        .worker
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("camera is not open"))?
                        .stop_video();
                    if let Err(error) = stopped {
                        self.reconnect_required = true;
                        return Err(hardware(error));
                    }
                    self.pending = None;
                    self.frame = None;
                    self.video_active = false;
                }
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
    crate::asi::continuous::serve(
        |method, params| {
            transport::require_sdk_absent()?;
            host.command(method, &params)
        },
        error_details,
    )
}

fn error_details(error: &anyhow::Error) -> Value {
    json!({
        "sdkCode":if error.is::<HardwareFailure>() { None } else {Some(8)},
        "sdkOperation":"direct", "transportFailure":crate::asi::direct::transfer::Failure::details(error),
        "controlUncertain":matches!(error.downcast_ref::<super::environment::CoolingError>(),Some(super::environment::CoolingError::Uncertain(_)))
    })
}

#[cfg(test)]
mod cooling_tests {
    use super::*;

    fn capture(mode: &str) {
        let mut host = Host {
            simulate: true,
            ..Host::default()
        };
        host.command("open", &json!({"name":"ZWO ASI585MM Pro"}))
            .unwrap();
        host.command(
            "start",
            &json!({"mode":mode,"maxFps":120.0,"width":64,"height":64,
            "x":0,"y":0,"bin":1,"microseconds":6_000_000,"dark":false}),
        )
        .unwrap();
        for (control, value) in [(16, -10), (17, 1), (16, -15)] {
            host.command("set", &json!({"control":control,"value":value}))
                .unwrap();
            assert_eq!(
                host.command("get", &json!({"control":control})).unwrap().0,
                value
            );
            assert_eq!(host.command("status", &Value::Null).unwrap().0, 1);
        }
        for (control, value) in [(16, 31), (17, 2), (8, 0), (0, 10), (21, 1)] {
            assert!(
                host.command("set", &json!({"control":control,"value":value}))
                    .is_err()
            );
        }
        assert_eq!(host.command("get", &json!({"control":16})).unwrap().0, -15);
        assert_eq!(host.command("get", &json!({"control":17})).unwrap().0, 1);
        for control in [8, 15] {
            assert!(
                host.command("get", &json!({"control":control}))
                    .unwrap()
                    .0
                    .is_i64()
            );
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while host.command("status", &Value::Null).unwrap().0 == 1 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        // Retaining a completed image also permits cooling without consuming it.
        host.command("set", &json!({"control":17,"value":0}))
            .unwrap();
        assert_eq!(host.command("get", &json!({"control":17})).unwrap().0, 0);
        assert_eq!(host.command("status", &Value::Null).unwrap().0, 2);
        let (metadata, pixels) = host.command("download", &Value::Null).unwrap();
        assert_eq!(metadata["mode"], mode);
        assert_eq!(
            pixels,
            (0..4096u16).flat_map(u16::to_le_bytes).collect::<Vec<_>>()
        );
        if mode == "video" {
            assert_eq!(metadata["deliveredFrames"], 1);
        }
        host.command("close", &Value::Null).unwrap();
    }
    #[test]
    fn acknowledged_cooling_during_still_and_retained_frame() {
        capture("still");
    }
    #[test]
    fn acknowledged_cooling_during_video_and_retained_frame() {
        capture("video");
    }
    #[test]
    fn uncooled_camera_rejects_cooling_and_errors_preserve_uncertainty() {
        let mut host = Host {
            simulate: true,
            ..Host::default()
        };
        host.command("open", &json!({"name":"ZWO ASI662MC"}))
            .unwrap();
        for control in [16, 17] {
            assert!(
                host.command("set", &json!({"control":control,"value":0}))
                    .is_err()
            );
        }
        host.command("close", &Value::Null).unwrap();
        let uncertain = hardware(anyhow::Error::new(
            super::super::environment::CoolingError::Uncertain("USB write failed".into()),
        ));
        assert_eq!(error_details(&uncertain)["controlUncertain"], true);
        assert_eq!(error_details(&uncertain)["sdkCode"], Value::Null);
        let unsent = hardware(anyhow::Error::new(
            super::super::environment::CoolingError::Expired,
        ));
        assert_eq!(error_details(&unsent)["controlUncertain"], false);
    }
}

#[cfg(test)]
mod video_tests {
    use super::*;
    fn parameters() -> Value {
        json!({"mode":"video","maxFps":120.0,"width":64,"height":64,
            "x":0,"y":0,"bin":1,"microseconds":1000,"dark":false})
    }
    fn open() -> Host {
        let mut host = Host {
            simulate: true,
            ..Host::default()
        };
        let (result, _) = host
            .command("open", &json!({"name":"ZWO ASI662MC"}))
            .unwrap();
        assert_eq!(result["captureModes"], json!(["still", "video"]));
        assert_eq!(result["videoFrameRecoveryAttempts"], 1);
        host
    }
    fn frame(host: &mut Host, params: &Value) -> Frame {
        host.command("start", params).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while host.command("status", &Value::Null).unwrap().0 == 1 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        host.command("download", &Value::Null).unwrap()
    }
    #[test]
    fn video_reuses_session_reconfigures_and_preserves_still_default() {
        let mut host = open();
        let mut params = parameters();
        let (first, pixels) = frame(&mut host, &params);
        assert_eq!(first["deliveredFrames"], 1);
        assert_eq!(pixels.len(), 64 * 64 * 2);
        assert_eq!(frame(&mut host, &params).0["deliveredFrames"], 2);
        params["maxFps"] = json!(100.0);
        assert_eq!(frame(&mut host, &params).0["deliveredFrames"], 3);
        params["microseconds"] = json!(2000);
        assert_eq!(frame(&mut host, &params).0["deliveredFrames"], 1);
        params.as_object_mut().unwrap().remove("mode");
        assert_eq!(frame(&mut host, &params).0["mode"], "still");
        assert!(!host.video_active);
        host.command("close", &Value::Null).unwrap();
    }
    #[test]
    fn one_bad_video_envelope_is_recovered_without_faulting_the_protocol() {
        let mut host = open();
        host.command("simulation", &json!({"framingFailures":1}))
            .unwrap();
        let (metadata, pixels) = frame(&mut host, &parameters());
        assert_eq!(metadata["framingRecoveries"], 1);
        assert_eq!(pixels.len(), 8192);
        host.command("simulation", &json!({"framingFailures":0}))
            .unwrap();
        assert_eq!(frame(&mut host, &parameters()).0["framingRecoveries"], 0);
        host.command("close", &Value::Null).unwrap();
    }
    #[test]
    fn repeated_bad_video_envelopes_still_fault_instead_of_looping() {
        let mut host = open();
        host.command("simulation", &json!({"framingFailures":2}))
            .unwrap();
        host.command("start", &parameters()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match host.command("status", &Value::Null) {
                Ok((status, _)) => assert_eq!(status, 1),
                Err(error) => {
                    assert!(format!("{error:#}").contains("after one stream restart"));
                    break;
                }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(host.command("download", &Value::Null).is_err());
        host.command("close", &Value::Null).unwrap();
    }
    #[test]
    fn stop_interrupts_the_recovery_fps_wait_and_allows_a_new_capture() {
        let mut host = open();
        host.command("simulation", &json!({"framingFailures":1}))
            .unwrap();
        let mut params = parameters();
        params["maxFps"] = json!(0.01);
        host.command("start", &params).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(host.command("status", &Value::Null).unwrap().0, 1);
        let started = Instant::now();
        host.command("stop", &Value::Null).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        host.command("simulation", &json!({"framingFailures":0}))
            .unwrap();
        assert_eq!(frame(&mut host, &parameters()).0["framingRecoveries"], 0);
        host.command("close", &Value::Null).unwrap();
    }
    #[test]
    fn stop_cancels_exposure_and_low_fps_wait_then_can_restart() {
        let mut host = open();
        let mut params = parameters();
        params["microseconds"] = json!(30_000_000);
        host.command("start", &params).unwrap();
        let began = Instant::now();
        host.command("stop", &Value::Null).unwrap();
        assert!(began.elapsed() < Duration::from_secs(2));
        assert_eq!(host.command("status", &Value::Null).unwrap().0, 0);
        params = parameters();
        params["maxFps"] = json!(0.01);
        frame(&mut host, &params);
        host.command("start", &params).unwrap();
        let began = Instant::now();
        host.command("stop", &Value::Null).unwrap();
        assert!(began.elapsed() < Duration::from_secs(2));
        frame(&mut host, &parameters());
        host.command("close", &Value::Null).unwrap();
    }
    #[test]
    fn asi676_video_uses_its_own_geometry_and_restarts_cleanly() {
        let mut host = Host {
            simulate: true,
            ..Host::default()
        };
        let (caps, _) = host
            .command("open", &json!({"name":"ZWO ASI676MC"}))
            .unwrap();
        assert_eq!(caps["captureModes"], json!(["still", "video"]));
        assert_eq!(caps["videoFrameRecoveryAttempts"], 1);
        let mut params = parameters();
        params["x"] = json!(2); // Valid for 676, not the 662's 8-pixel alignment.
        params["y"] = json!(2);
        assert_eq!(frame(&mut host, &params).0["deliveredFrames"], 1);
        assert_eq!(frame(&mut host, &params).0["deliveredFrames"], 2);
        host.command("stop", &Value::Null).unwrap();
        assert_eq!(frame(&mut host, &params).0["deliveredFrames"], 1);
        host.command("close", &Value::Null).unwrap();
        let mut unsupported = Host {
            simulate: true,
            ..Host::default()
        };
        for model in Model::ALL
            .into_iter()
            .filter(|m| m.video_profile().is_none())
        {
            let (caps, _) = unsupported
                .command("open", &json!({"name":model.name()}))
                .unwrap();
            assert_eq!(caps["captureModes"], json!(["still"]));
            assert_eq!(caps["videoFrameRecoveryAttempts"], 0);
            assert!(unsupported.command("start", &parameters()).is_err());
            unsupported.command("close", &Value::Null).unwrap();
        }
    }

    #[test]
    fn invalid_video_requests_do_not_change_active_state() {
        let mut host = open();
        for (key, value) in [
            ("mode", json!("typo")),
            ("mode", Value::Null),
            ("maxFps", json!(0)),
            ("maxFps", json!("1")),
            ("dark", json!(true)),
            ("microseconds", json!(30_000_001)),
            ("bin", json!(2)),
            ("readRetries", json!(1)),
            ("captureTimeoutSeconds", json!(-1)),
        ] {
            let mut params = parameters();
            params[key] = value;
            assert!(host.command("start", &params).is_err(), "{key}");
            assert!(!host.video_active);
            assert!(host.pending.is_none());
        }
        host.command("close", &Value::Null).unwrap();
        host.command("open", &json!({"name":"ZWO ASI2600MM Duo"}))
            .unwrap();
        assert!(host.command("start", &parameters()).is_err());
        host.command("close", &Value::Null).unwrap();
    }
}

#[cfg(test)]
mod usb2_inventory_tests {
    use super::*;

    #[test]
    fn every_advertised_model_has_a_usb2_descriptor_path() {
        // Use the production inventory: adding a model must extend coverage,
        // rather than silently falling outside a separate hard-coded PID list.
        // This is descriptor/contract coverage, NOT a sensor capture simulation.
        let mut device = [0; 18];
        device[0] = 18;
        device[1] = 1;
        device[2..4].copy_from_slice(&0x0210_u16.to_le_bytes());
        device[8..10].copy_from_slice(&0x03c3_u16.to_le_bytes());
        let config = [
            9, 2, 25, 0, 1, 1, 0, 0x80, 0, 9, 4, 0, 0, 1, 0xff, 0, 0, 0, 7, 5, 0x81, 2, 0, 2, 0,
        ];
        for model in Model::ALL {
            device[10..12].copy_from_slice(&(model.pid() as u16).to_le_bytes());
            let info = super::super::protocol::describe(&device, &config, 0).unwrap();
            assert_eq!(
                super::super::link::validate(&info, model.pid()).unwrap(),
                super::super::link::Link::HighSpeed,
                "{}",
                model.name()
            );
            let descriptor = model.descriptor();
            let controls = model.controls(false);
            let offset = controls.iter().find(|c| c["type"] == 5).unwrap()["value"]
                .as_u64()
                .unwrap() as u32;
            for microseconds in [32, 100_000, 999_999, 1_000_000] {
                for (width, height) in [
                    (64, 64),
                    (
                        descriptor["width"].as_u64().unwrap() as u32,
                        descriptor["height"].as_u64().unwrap() as u32,
                    ),
                ] {
                    let settings = Settings {
                        width,
                        height,
                        microseconds,
                        offset,
                        ..Settings::default()
                    };
                    let result = model.validate(&settings, 0, 1);
                    if model == Model::Guide && microseconds == 32 {
                        // Existing guide-specific zero-line restriction must
                        // not disappear in a blanket USB2 compatibility change.
                        assert!(result.unwrap_err().to_string().contains("zero-line"));
                    } else {
                        result.unwrap();
                    }
                }
            }
            assert_eq!(descriptor["retainedFrameReads"], model != Model::Guide);
        }
    }
}
