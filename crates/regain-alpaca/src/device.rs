use crate::profile::{Profile, Profiles};
use anyhow::{Result, ensure};
use regain_core::{
    CancellationToken, Diagnostic, Exposure, Frame, Runtime, Selection, Session, SharedStatus,
    Status,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::sync::Mutex as AsyncMutex;

#[derive(Debug)]
pub struct Error(pub i32, pub String);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}
impl std::error::Error for Error {}
pub fn error(code: i32, message: impl Into<String>) -> anyhow::Error {
    Error(code, message.into()).into()
}
pub fn unsupported(member: &str) -> anyhow::Error {
    error(0x400, format!("{member} is not supported"))
}
pub struct Device {
    pub slot: usize,
    pub profiles: Arc<Profiles>,
    runtime: Runtime,
    log: Diagnostic,
    engine: AsyncMutex<Option<Session>>,
    state: Mutex<State>,
    connection: AsyncMutex<()>,
    idle: tokio::sync::Notify,
}
struct State {
    runtime_options: Option<regain_core::camera_options::CameraRuntimeHandle>,
    clients: HashSet<(bool, u32)>,
    status: Option<SharedStatus>,
    changing: bool,
    connection_error: Option<String>,
    busy: bool,
    wheel_busy: bool,
    frame: Option<Arc<Frame>>,
    error: Option<String>,
    cancel: CancellationToken,
    bin: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    started: Instant,
    duration: f64,
    closed: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            runtime_options: None,
            clients: HashSet::new(),
            status: None,
            changing: false,
            connection_error: None,
            busy: false,
            wheel_busy: false,
            frame: None,
            error: None,
            cancel: CancellationToken::new(),
            bin: 1,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            started: Instant::now(),
            duration: 0.,
            closed: false,
        }
    }
}
struct WheelCommand<'a>(&'a Device);
impl Drop for WheelCommand<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().wheel_busy = false;
    }
}
impl Device {
    pub fn new(
        slot: usize,
        profiles: Arc<Profiles>,
        runtime: Runtime,
        log: Diagnostic,
    ) -> Arc<Self> {
        Arc::new(Self {
            slot,
            profiles,
            runtime,
            log,
            engine: AsyncMutex::new(None),
            state: Mutex::new(State::default()),
            connection: AsyncMutex::new(()),
            idle: tokio::sync::Notify::new(),
        })
    }
    pub fn in_use(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.status.is_some() || s.changing
    }
    pub fn configure(&self, p: Profile) -> Result<()> {
        let s = self.state.lock().unwrap();
        ensure!(
            !s.closed && !s.changing,
            Error(0x40B, "Disconnect all clients before changing setup".into())
        );
        if s.status.is_some() {
            let old = self.profiles.get(self.slot)?;
            let mut frozen = p.clone();
            let options =
                regain_core::camera_options::CameraRuntimeOptions::from_recovery(&p.recovery);
            options.validate()?;
            regain_core::camera_options::CameraRuntimeOptions::from_recovery(&old.recovery)
                .apply(&mut frozen.recovery);
            ensure!(
                serde_json::to_value(&frozen)? == serde_json::to_value(&old)?,
                Error(
                    0x40B,
                    "Only transfer logging and cooler power limit can change while connected"
                        .into()
                )
            );
            self.profiles.save(self.slot, p)?;
            s.runtime_options
                .as_ref()
                .ok_or_else(|| error(0x40B, "Camera settings unavailable"))?
                .update(options)?;
            return Ok(());
        }
        self.profiles.save(self.slot, p)
    }
    pub fn snapshot(&self) -> Option<Status> {
        self.state
            .lock()
            .unwrap()
            .status
            .as_ref()
            .map(|s| s.lock().unwrap().clone())
    }
    pub async fn connected(
        self: &Arc<Self>,
        client: u32,
        on: bool,
        asynchronous: bool,
    ) -> Result<()> {
        self.connected_role(client, on, asynchronous, false).await
    }
    async fn connected_role(
        self: &Arc<Self>,
        client: u32,
        on: bool,
        asynchronous: bool,
        wheel: bool,
    ) -> Result<()> {
        {
            let mut s = self.state.lock().unwrap();
            ensure!(
                !s.changing && !s.closed,
                Error(0x40B, "Connection change already in progress".into())
            );
            s.changing = true;
            s.connection_error = None;
        }
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let result = owner.change(client, on, wheel).await;
            {
                let mut s = owner.state.lock().unwrap();
                s.changing = false;
                s.connection_error = result.as_ref().err().map(|e| format!("{e:#}"));
            }
            result
        });
        if asynchronous { Ok(()) } else { task.await? }
    }
    async fn change(self: &Arc<Self>, client: u32, on: bool, wheel: bool) -> Result<()> {
        let _gate = self.connection.lock().await;
        if !on {
            {
                let mut s = self.state.lock().unwrap();
                if !s.clients.remove(&(wheel, client)) {
                    return Ok(());
                }
                if !s.clients.is_empty() && (wheel || s.clients.iter().any(|(w, _)| !w)) {
                    return Ok(());
                }
                s.cancel.cancel();
            }
            let mut engine = self.engine.lock().await;
            if !self.state.lock().unwrap().clients.is_empty() {
                // Last camera client left; the wheel keeps the transport alive.
                if let Some(session) = engine.as_mut() {
                    session
                        .disable_thermal_outputs(&CancellationToken::new())
                        .await?;
                }
                return Ok(());
            }
            if let Some(mut session) = engine.take() {
                session.close().await;
            }
            let mut s = self.state.lock().unwrap();
            s.status = None;
            s.frame = None;
            s.error = None;
            s.busy = false;
            return Ok(());
        }
        if wheel && self.snapshot().is_some() {
            self.engine
                .lock()
                .await
                .as_mut()
                .ok_or_else(|| error(0x407, "Camera unavailable"))?
                .filter_wheel_status(&CancellationToken::new())
                .await?;
        }
        let first_camera = {
            let s = self.state.lock().unwrap();
            !wheel && s.status.is_some() && !s.clients.iter().any(|(w, _)| !w)
        };
        if first_camera {
            let mut engine = self.engine.lock().await;
            let session = engine
                .as_mut()
                .ok_or_else(|| error(0x407, "Camera unavailable"))?;
            for (k, v) in &self.profiles.get(self.slot)?.controls {
                Session::queue_control(&session.status, *k, *v)?;
            }
            session.refresh(&CancellationToken::new()).await?;
        }
        {
            let mut s = self.state.lock().unwrap();
            if s.status.is_some() {
                s.clients.insert((wheel, client));
                return Ok(());
            }
        }
        let p = self.profiles.get(self.slot)?;
        let camera = p
            .camera
            .as_ref()
            .ok_or_else(|| error(0x40B, "Select a camera on the setup page first"))?;
        let selection = Selection {
            name: camera["name"].as_str().unwrap_or("").into(),
            serial: p.serial.clone(),
            direct: p.direct,
            sdk_fallback: p.sdk_fallback,
            recovery: p.recovery.clone(),
        };
        let mut session = Session::new(selection, self.runtime.clone(), self.log.clone())?;
        let token = CancellationToken::new();
        let result = async {
            session.connect(&token).await?;
            if wheel || p.filter_wheel.enabled {
                session.filter_wheel_status(&token).await?;
            }
            for (k, v) in &p.controls {
                if wheel && matches!(k, 16 | 17 | 21) {
                    continue;
                }
                Session::queue_control(&session.status, *k, *v)?;
            }
            session.refresh(&token).await?;
            let snapshot = session.snapshot();
            self.profiles.save(
                self.slot,
                Profile {
                    serial: snapshot.serial.clone(),
                    camera: Some(snapshot.info.clone()),
                    ..p
                },
            )?;
            let mut s = self.state.lock().unwrap();
            s.status = Some(session.status.clone());
            s.runtime_options = Some(session.runtime_options());
            s.clients.insert((wheel, client));
            s.error = None;
            s.frame = None;
            s.bin = 1;
            s.x = 0;
            s.y = 0;
            s.width = snapshot.info["width"].as_u64().unwrap_or(0) as u32 / 8 * 8;
            s.height = snapshot.info["height"].as_u64().unwrap_or(0) as u32 / 2 * 2;
            Result::<()>::Ok(())
        }
        .await;
        if let Err(error) = result {
            session.close().await;
            return Err(error);
        }
        *self.engine.lock().await = Some(session);
        Ok(())
    }
    fn reserve_wheel(&self, client: u32) -> Result<WheelCommand<'_>> {
        let mut s = self.state.lock().unwrap();
        ensure!(
            s.clients.contains(&(true, client)),
            Error(0x407, "Wheel client is not connected".into())
        );
        ensure!(
            !s.busy && !s.wheel_busy && !s.changing,
            Error(
                0x40B,
                "Cannot move the filter wheel while camera or wheel is busy".into()
            )
        );
        s.wheel_busy = true;
        Ok(WheelCommand(self))
    }
    fn cached_wheel(&self) -> Result<Value> {
        let s = self.state.lock().unwrap();
        let state = s
            .status
            .as_ref()
            .ok_or_else(|| error(0x407, "Camera unavailable"))?
            .lock()
            .unwrap();
        ensure!(
            state.control_connection_available && !state.filter_wheel.is_null(),
            Error(
                0x40B,
                "Wheel position unavailable during camera recovery".into()
            )
        );
        let mut wheel = state.filter_wheel.clone();
        if s.wheel_busy {
            wheel["moving"] = json!(true);
            wheel["verified"] = json!(false);
            wheel["position"] = json!(-1);
        }
        Ok(wheel)
    }
    pub async fn wheel_request(
        self: &Arc<Self>,
        member: &str,
        put: bool,
        p: &Params,
        client: u32,
    ) -> Result<Value> {
        if put && matches!(member, "connected" | "connect" | "disconnect") {
            let on = if member == "connected" {
                p.boolean("Connected")?
            } else {
                member == "connect"
            };
            self.connected_role(client, on, member != "connected", true)
                .await?;
            return Ok(Value::Null);
        }
        if !put {
            match member {
                "name" => {
                    return Ok(json!(format!(
                        "QHY camera filter wheel  -  {}",
                        self.profiles.get(self.slot)?.camera_name()
                    )));
                }
                "description" => {
                    return Ok(json!(
                        "QHY 4-pin filter wheel sharing the Regain camera connection"
                    ));
                }
                "driverinfo" => {
                    return Ok(json!(
                        "PulsarFab regain QHY camera bridge; positions verified before and after capture"
                    ));
                }
                "driverversion" => return Ok(json!(env!("CARGO_PKG_VERSION"))),
                "interfaceversion" => return Ok(json!(3)),
                "supportedactions" => return Ok(json!(["Regain.Diagnostics"])),
                "connected" => {
                    return Ok(json!(
                        self.state.lock().unwrap().clients.contains(&(true, client))
                    ));
                }
                "connecting" => return self.get("connecting", client),
                _ => (),
            }
        }
        let busy = {
            let s = self.state.lock().unwrap();
            ensure!(
                s.clients.contains(&(true, client)),
                Error(0x407, "Wheel client is not connected".into())
            );
            s.busy
        };
        if put && member != "position" {
            if member == "action"
                && p.string("Action")?
                    .eq_ignore_ascii_case("Regain.Diagnostics")
            {
                return Ok(json!(serde_json::to_string(
                    &self.snapshot().map(|s| s.filter_wheel)
                )?));
            }
            return Err(unsupported(member));
        }
        let state = if put {
            // Reserve admission before waiting for idle telemetry. Otherwise a
            // racing exposure could overtake this request and defer a motor move
            // until after a capture that the caller thought it would precede.
            let _admission = self.reserve_wheel(client)?;
            let mut engine = self.engine.lock().await;
            let session = engine
                .as_mut()
                .ok_or_else(|| error(0x407, "Camera unavailable"))?;
            let position = p.integer("Position")?;
            ensure!(
                (0..16).contains(&position),
                Error(0x401, "Invalid filter position".into())
            );
            session
                .filter_wheel_move(position as u8, &CancellationToken::new())
                .await?
        } else if !busy {
            // Polling may return the existing verified/ moving snapshot while
            // telemetry or capture owns the transport; it never queues behind
            // a long exposure and never fabricates a settled position.
            if let Ok(mut engine) = self.engine.try_lock() {
                let pending = {
                    let s = self.state.lock().unwrap();
                    s.busy || s.wheel_busy
                };
                if pending {
                    self.cached_wheel()?
                } else {
                    engine
                        .as_mut()
                        .ok_or_else(|| error(0x407, "Camera unavailable"))?
                        .filter_wheel_status(&CancellationToken::new())
                        .await?
                }
            } else {
                self.cached_wheel()?
            }
        } else {
            self.cached_wheel()?
        };
        if put {
            return Ok(Value::Null);
        }
        let slots = state["slots"]
            .as_u64()
            .ok_or_else(|| error(0x40B, "Wheel slot count unavailable"))?
            as usize;
        let profile = self.profiles.get(self.slot)?.filter_wheel;
        Ok(match member {
            "position" => {
                if state["moving"] == true {
                    json!(-1)
                } else {
                    state["position"].clone()
                }
            }
            "names" => json!(
                (0..slots)
                    .map(|i| profile
                        .names
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| format!("Filter {}", i + 1)))
                    .collect::<Vec<_>>()
            ),
            "focusoffsets" => json!(
                (0..slots)
                    .map(|i| profile.focus_offsets.get(i).copied().unwrap_or(0))
                    .collect::<Vec<_>>()
            ),
            "devicestate" => {
                json!([{"Name":"Position","Value":if state["moving"]==true {json!(-1)}else{state["position"].clone()}}])
            }
            _ => return Err(unsupported(member)),
        })
    }
    pub async fn refresh(&self) {
        if let Ok(mut owner) = self.engine.try_lock()
            && let Some(session) = owner.as_mut()
            && self.in_use()
        {
            let token = CancellationToken::new();
            if let Err(error) = session.refresh(&token).await {
                (self.log)("warning", "controls.failed", &format!("{error:#}"));
            }
        }
    }
    fn check(s: &State, client: u32) -> Result<Status> {
        ensure!(
            s.clients.contains(&(false, client)) && s.status.is_some(),
            Error(0x407, "This client is not connected".into())
        );
        Ok(s.status.as_ref().unwrap().lock().unwrap().clone())
    }
    pub fn get(&self, member: &str, client: u32) -> Result<Value> {
        let s = self.state.lock().unwrap();
        match member {
            "name" => {
                if let Some(status) = &s.status {
                    return Ok(status.lock().unwrap().info["name"].clone());
                }
                return Ok(json!(self.profiles.get(self.slot)?.camera_name()));
            }
            "description" => return Ok(json!("Camera driver with automatic retries")),
            "driverinfo" => {
                let recovery = s
                    .status
                    .as_ref()
                    .map(|status| status.lock().unwrap().recovery_info())
                    .unwrap_or_else(|| Status::default().recovery_info());
                return Ok(json!(format!(
                    "PulsarFab regain {} / Rust camera recovery; {}",
                    env!("CARGO_PKG_VERSION"),
                    recovery
                )));
            }
            "driverversion" => return Ok(json!(env!("CARGO_PKG_VERSION"))),
            "interfaceversion" => return Ok(json!(4)),
            "connected" => return Ok(json!(s.clients.contains(&(false, client)))),
            "connecting" => {
                if let Some(e) = &s.connection_error {
                    return Err(error(0x500, e));
                }
                return Ok(json!(s.changing));
            }
            "supportedactions" => {
                return Ok(json!([
                    "Regain.Diagnostics",
                    "Regain.Controls",
                    "Regain.SetControl"
                ]));
            }
            _ => {}
        }
        let status = Self::check(&s, client)?;
        let info = &status.info;
        let cap = |k| {
            status
                .controls
                .get(&k)
                .ok_or_else(|| unsupported(&format!("Control {k}")))
        };
        let val = |k| -> Result<i64> {
            cap(k)?;
            Ok(*status.values.get(&k).unwrap_or(&0))
        };
        let writable = |k| status.controls.get(&k).is_some_and(|c| c.writable);
        let camera_state = if s.busy {
            match status.phase.as_str() {
                "Downloading" => 4,
                "Starting exposure" | "Exposing" => 2,
                _ => 1,
            }
        } else if s.error.is_some() {
            5
        } else {
            0
        };
        Ok(match member {
            "cameraxsize" => info["width"].clone(),
            "cameraysize" => info["height"].clone(),
            "pixelsizex" | "pixelsizey" => info["pixelSize"].clone(),
            "sensorname" => info["name"].clone(),
            "hasshutter" => info["shutter"].clone(),
            "sensortype" => json!(if info["color"] == true { 2 } else { 0 }),
            "bayeroffsetx" | "bayeroffsety" => {
                if info["color"] != true {
                    return Err(unsupported(member));
                }
                let b = info["bayer"].as_u64().unwrap_or(0);
                json!(if if member == "bayeroffsetx" {
                    b == 1 || b == 2
                } else {
                    b == 1 || b == 3
                } {
                    1
                } else {
                    0
                })
            }
            "canabortexposure" => json!(true),
            "canasymmetricbin" | "canfastreadout" | "canpulseguide" | "canstopexposure" => {
                json!(false)
            }
            "cansetccdtemperature" => json!(writable(16) && writable(17)),
            "cangetcoolerpower" => json!(status.controls.contains_key(&15)),
            "ccdtemperature" => json!(val(8)? as f64 / 10.),
            "setccdtemperature" => json!(val(16)?),
            "cooleron" => json!(val(17)? != 0),
            "coolerpower" => json!(val(15)?),
            "gain" => json!(val(0)?),
            "gainmin" => json!(cap(0)?.min),
            "gainmax" => json!(cap(0)?.max),
            "offset" => json!(val(5)?),
            "offsetmin" => json!(cap(5)?.min),
            "offsetmax" => json!(cap(5)?.max),
            "maxadu" => json!(65535),
            "maxbinx" | "maxbiny" => json!(
                info["bins"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_u64)
                    .max()
                    .unwrap_or(1)
            ),
            "binx" | "biny" => json!(s.bin),
            "startx" => json!(s.x),
            "starty" => json!(s.y),
            "numx" => json!(s.width),
            "numy" => json!(s.height),
            "exposuremin" => json!(cap(1)?.min as f64 / 1e6),
            "exposuremax" => json!(cap(1)?.max as f64 / 1e6),
            "exposureresolution" => json!(0.000001),
            "readoutmode" => json!(0),
            "readoutmodes" => json!(["RAW16"]),
            "camerastate" => json!(camera_state),
            "imageready" => {
                if let Some(e) = &s.error {
                    return Err(error(0x500, format!("Capture failed: {e}")));
                }
                json!(!s.busy && s.frame.is_some())
            }
            "percentcompleted" => json!(if s.busy {
                (s.started.elapsed().as_secs_f64() / s.duration.max(0.001) * 100.).clamp(0., 99.)
                    as u32
            } else if s.frame.is_some() {
                100
            } else {
                0
            }),
            "lastexposureduration" => {
                json!(Self::image_locked(&s)?.exposure.microseconds as f64 / 1e6)
            }
            "lastexposurestarttime" => {
                let frame = Self::image_locked(&s)?;
                let start = chrono::DateTime::parse_from_rfc3339(
                    frame.metadata["startedUtc"].as_str().unwrap_or(""),
                )?;
                json!(
                    start
                        .with_timezone(&chrono::Utc)
                        .format("%Y-%m-%dT%H:%M:%S")
                        .to_string()
                )
            }
            "devicestate" => {
                json!([{"Name":"CameraState","Value":camera_state},{"Name":"ImageReady","Value":!s.busy&&s.frame.is_some()}])
            }
            _ => return Err(unsupported(member)),
        })
    }
    fn image_locked(s: &State) -> Result<Arc<Frame>> {
        if let Some(e) = &s.error {
            return Err(error(0x500, format!("Capture failed: {e}")));
        }
        ensure!(
            !s.busy,
            Error(0x402, "No completed image is available".into())
        );
        s.frame
            .clone()
            .ok_or_else(|| error(0x402, "No completed image is available"))
    }
    pub fn image(&self, client: u32) -> Result<Arc<Frame>> {
        let s = self.state.lock().unwrap();
        Self::check(&s, client)?;
        Self::image_locked(&s)
    }
    pub fn put(self: &Arc<Self>, member: &str, p: &Params, client: u32) -> Result<Value> {
        let mut s = self.state.lock().unwrap();
        let status = Self::check(&s, client)?;
        ensure!(
            !s.changing,
            Error(0x40B, "Connection change in progress".into())
        );
        ensure!(
            member != "startexposure" || !s.wheel_busy,
            Error(0x40B, "Filter wheel command is in progress".into())
        );
        if member == "abortexposure" {
            if s.busy {
                s.cancel.cancel()
            }
            return Ok(Value::Null);
        }
        if member == "action" {
            let name = crate::branding::action_name(p.string("Action")?);
            let args = p.string("Parameters")?;
            return match name.as_str() {
                "regain.diagnostics" => Ok(json!(serde_json::to_string(&status)?)),
                "regain.controls" => Ok(json!(serde_json::to_string(
                    &status
                        .controls
                        .values()
                        .map(|c| {
                            let mut c = c.clone();
                            c.value = *status.values.get(&c.kind).unwrap_or(&c.value);
                            c
                        })
                        .collect::<Vec<_>>()
                )?)),
                "regain.setcontrol" => {
                    ensure!(!s.busy, Error(0x40B, "Exposure active".into()));
                    let v: Value = serde_json::from_str(args)?;
                    let k = v["control"]
                        .as_i64()
                        .ok_or_else(|| error(0x401, "Missing control"))?;
                    ensure!(
                        matches!(k, 0 | 5 | 6 | 16 | 17 | 21 | 22 | 23),
                        Error(0x401, "Unsupported control".into())
                    );
                    Session::queue_control(
                        s.status.as_ref().unwrap(),
                        k as i32,
                        v["value"]
                            .as_i64()
                            .ok_or_else(|| error(0x401, "Missing value"))?,
                    )?;
                    Ok(json!(""))
                }
                _ => Err(error(0x40C, "Unknown action")),
            };
        }
        if matches!(
            member,
            "stopexposure"
                | "pulseguide"
                | "commandblind"
                | "commandbool"
                | "commandstring"
                | "fastreadout"
                | "subexposureduration"
        ) {
            return Err(unsupported(member));
        }
        ensure!(
            !s.busy || matches!(member, "cooleron" | "setccdtemperature"),
            Error(0x40B, "An exposure is already active".into())
        );
        match member {
            "binx" | "biny" => {
                let b = p.integer(member)?;
                ensure!(
                    status.info["bins"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|v| v.as_i64() == Some(b)),
                    Error(0x401, "Unsupported binning".into())
                );
                s.bin = b as u32;
                s.x = 0;
                s.y = 0;
                s.width = status.info["width"].as_u64().unwrap() as u32 / s.bin / 8 * 8;
                s.height = status.info["height"].as_u64().unwrap() as u32 / s.bin / 2 * 2;
            }
            "startx" | "starty" | "numx" | "numy" => {
                let v = p.integer(member)?;
                let origin = member.starts_with("start");
                let max = status.info[if member.ends_with('x') {
                    "width"
                } else {
                    "height"
                }]
                .as_u64()
                .unwrap() as i64
                    / s.bin as i64;
                ensure!(
                    v >= if origin { 0 } else { 1 } && v <= max - i64::from(origin),
                    Error(0x401, "ROI is outside the sensor".into())
                );
                match member {
                    "startx" => s.x = v as u32,
                    "starty" => s.y = v as u32,
                    "numx" => s.width = v as u32,
                    _ => s.height = v as u32,
                }
            }
            "gain" | "offset" | "cooleron" | "setccdtemperature" => {
                let (k, v) = match member {
                    "gain" => (0, p.integer("Gain")?),
                    "offset" => (5, p.integer("Offset")?),
                    "cooleron" => (17, i64::from(p.boolean("CoolerOn")?)),
                    _ => (16, {
                        let v = p.number("SetCCDTemperature")?;
                        let c = status
                            .controls
                            .get(&16)
                            .ok_or_else(|| unsupported(member))?;
                        ensure!(
                            v >= c.min as f64 && v <= c.max as f64,
                            Error(0x401, "Temperature target outside camera range".into())
                        );
                        v.round() as i64
                    }),
                };
                Session::queue_control(s.status.as_ref().unwrap(), k, v)?;
            }
            "readoutmode" => ensure!(
                p.integer("ReadoutMode")? == 0,
                Error(0x401, "Only RAW16 is supported".into())
            ),
            "startexposure" => {
                let seconds = p.number("Duration")?;
                let light = p.boolean("Light")?;
                let cap = status.controls.get(&1).unwrap();
                ensure!(
                    seconds >= 0.
                        && seconds <= cap.max as f64 / 1e6
                        && (seconds == 0. || seconds >= cap.min as f64 / 1e6),
                    Error(0x401, "Duration outside camera exposure range".into())
                );
                let e = Exposure {
                    width: s.width,
                    height: s.height,
                    bin: s.bin,
                    x: s.x,
                    y: s.y,
                    microseconds: ((seconds * 1e6).round() as u64).max(cap.min as u64),
                    dark: !light,
                };
                regain_core::validate_capture(
                    &status.info,
                    &status.controls,
                    &e,
                    status.backend == "direct" && self.profiles.get(self.slot)?.sdk_fallback,
                )?;
                s.frame = None;
                s.error = None;
                s.busy = true;
                s.started = Instant::now();
                s.duration = e.microseconds as f64 / 1e6;
                s.cancel = CancellationToken::new();
                let token = s.cancel.clone();
                let owner = self.clone();
                tokio::spawn(async move {
                    let mut engine = owner.engine.lock().await;
                    let result = match engine.as_mut() {
                        Some(session) => session.capture(e, &token).await,
                        None => Err(error(0x407, "Disconnected")),
                    };
                    let mut state = owner.state.lock().unwrap();
                    if !token.is_cancelled() {
                        match result {
                            Ok(frame) => state.frame = Some(Arc::new(frame)),
                            Err(e) => state.error = Some(format!("{e:#}")),
                        }
                    }
                    state.busy = false;
                    owner.idle.notify_waiters();
                });
            }
            _ => return Err(unsupported(member)),
        }
        Ok(Value::Null)
    }
    pub async fn abort(self: &Arc<Self>, client: u32) -> Result<()> {
        self.put("abortexposure", &Params::default(), client)?;
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.state.lock().unwrap().busy {
                return Ok(());
            }
            notified.await;
        }
    }
    pub async fn shutdown(&self) {
        {
            let mut s = self.state.lock().unwrap();
            s.closed = true;
            s.cancel.cancel();
        }
        let _gate = self.connection.lock().await;
        let mut engine = self.engine.lock().await;
        if let Some(mut session) = engine.take() {
            session.close().await;
        }
        let mut s = self.state.lock().unwrap();
        s.clients.clear();
        s.status = None;
        s.frame = None;
    }
}
#[derive(Default)]
pub struct Params(pub BTreeMap<String, String>);
impl Params {
    pub fn parse(text: &str) -> Result<Self> {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_str(text)?;
        let mut map = BTreeMap::new();
        for (k, v) in pairs {
            ensure!(
                map.insert(k.to_lowercase(), v).is_none(),
                Error(0x401, "Duplicate parameter".into())
            );
        }
        Ok(Self(map))
    }
    pub fn string(&self, name: &str) -> Result<&str> {
        self.0
            .get(&name.to_lowercase())
            .map(String::as_str)
            .ok_or_else(|| error(0x401, format!("Missing parameter {name}")))
    }
    pub fn integer(&self, name: &str) -> Result<i64> {
        self.string(name)?
            .parse()
            .map_err(|_| error(0x401, format!("Invalid integer: {name}")))
    }
    pub fn number(&self, name: &str) -> Result<f64> {
        let n: f64 = self
            .string(name)?
            .parse()
            .map_err(|_| error(0x401, format!("Invalid number: {name}")))?;
        ensure!(n.is_finite(), Error(0x401, "Number must be finite".into()));
        Ok(n)
    }
    pub fn boolean(&self, name: &str) -> Result<bool> {
        match self.string(name)?.to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(error(0x401, format!("Invalid boolean: {name}"))),
        }
    }
    pub fn optional_id(&self, name: &str) -> Result<u32> {
        self.0.get(&name.to_lowercase()).map_or(Ok(0), |v| {
            v.parse()
                .map_err(|_| error(0x401, "Invalid client or transaction ID"))
        })
    }
}

#[cfg(test)]
mod wheel_tests {
    use super::*;
    #[test]
    fn optical_command_admission_fences_exposure_and_is_released_on_error() {
        let device = Device::new(
            0,
            Arc::new(Profiles::new(None).unwrap()),
            Runtime {
                directory: "unused".into(),
                sdk: "unused".into(),
                simulate: true,
                sdk_simulation: None,
            },
            Arc::new(|_, _, _| {}),
        );
        {
            let mut s = device.state.lock().unwrap();
            s.clients.extend([(false, 7), (true, 7)]);
            s.status = Some(Arc::new(Mutex::new(Status {
                control_connection_available: true,
                filter_wheel: json!({"slots":9,"position":0,"moving":false,"verified":true}),
                ..Status::default()
            })));
        }
        let command = device.reserve_wheel(7).unwrap();
        assert_eq!(device.cached_wheel().unwrap()["position"], -1);
        let error = device
            .put("startexposure", &Params(BTreeMap::new()), 7)
            .unwrap_err();
        assert_eq!(error.downcast_ref::<Error>().unwrap().0, 0x40B);
        assert!(device.reserve_wheel(7).is_err());
        drop(command);
        assert_eq!(device.cached_wheel().unwrap()["position"], 0);
        device.state.lock().unwrap().busy = true;
        assert!(device.reserve_wheel(7).is_err());
        assert!(!device.state.lock().unwrap().wheel_busy);
    }
}
