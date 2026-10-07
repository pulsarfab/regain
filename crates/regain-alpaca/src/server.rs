use crate::{
    device::{Device, Error, Params, error},
    profile::{Profile, Profiles},
};
use anyhow::Result;
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, RawQuery, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use regain_core::{CancellationToken, Diagnostic, Failure, Frame, Runtime};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    convert::Infallible,
    io::Write,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};
use tokio::net::UdpSocket;

pub struct Server {
    pub hub: Option<Arc<crate::hub_output::Publisher>>,
    pub profiles: Arc<Profiles>,
    pub runtime: Runtime,
    rotators: Mutex<HashMap<usize, Arc<crate::rotator::Rotator>>>,
    pub filterwheel: crate::accessory::Accessory,
    focusers: Mutex<HashMap<usize, Arc<crate::accessory::Accessory>>>,
    pub flatpanel: crate::flatpanel::FlatPanel,
    pub log: Arc<Log>,
    devices: Mutex<HashMap<usize, Arc<Device>>>,
    transaction: AtomicU32,
    // Serializes enumeration with connection changes, and checks slot identity claims.
    connections: tokio::sync::Mutex<()>,
}
pub struct Log {
    events: Mutex<VecDeque<String>>,
    path: Option<PathBuf>,
}
impl Log {
    pub fn new(path: Option<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(VecDeque::new()),
            path,
        })
    }
    pub fn diagnostic(self: &Arc<Self>, slot: Option<usize>) -> Diagnostic {
        let owner = self.clone();
        Arc::new(move |level, event, message| {
            let text = format!(
                "{} {}{event}: {message}",
                chrono::Utc::now().to_rfc3339(),
                slot.map(|s| format!("Camera {s} ")).unwrap_or_default()
            );
            let record = json!({"version":1,"level":level,"event":event,"message":message,"pid":std::process::id()});
            let _ = writeln!(std::io::stderr().lock(), "REGAIN_DIAGNOSTIC {record}");
            if level == "debug" {
                return;
            }
            let mut events = owner.events.lock().unwrap();
            events.push_back(text.clone());
            while events.len() > 200 {
                events.pop_front();
            }
            if let Some(dir) = &owner.path {
                let _ = (|| -> std::io::Result<()> {
                    std::fs::create_dir_all(dir)?;
                    let path = dir.join(format!(
                        "regain-{}.log",
                        chrono::Utc::now().format("%Y-%m-%d")
                    ));
                    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 10 * 1024 * 1024) {
                        let old = path.with_extension("previous.log");
                        if old.exists() {
                            std::fs::remove_file(&old)?;
                        }
                        std::fs::rename(&path, old)?;
                    }
                    writeln!(
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)?,
                        "{level} {}",
                        text.replace(['\n', '\r'], " ")
                    )?;
                    Ok(())
                })();
            }
        })
    }
    pub fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().iter().cloned().collect()
    }
}
impl Server {
    pub fn new(profiles: Arc<Profiles>, runtime: Runtime, log: Arc<Log>) -> Arc<Self> {
        Self::with_hub(profiles, runtime, log, None)
    }
    pub fn with_hub(
        profiles: Arc<Profiles>,
        runtime: Runtime,
        log: Arc<Log>,
        hub: Option<Arc<crate::hub_output::Publisher>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            hub,
            flatpanel: crate::flatpanel::FlatPanel::new(
                profiles.accessory_path("ofp2"),
                runtime.directory.clone(),
                runtime.simulate,
            ),
            filterwheel: crate::accessory::Accessory::new(
                "efw",
                0,
                None,
                profiles.accessory_path("efw"),
                runtime.directory.clone(),
                runtime.simulate,
            ),
            focusers: Mutex::new(HashMap::new()),
            rotators: Mutex::new(HashMap::new()),
            profiles,
            runtime,
            log,
            devices: Mutex::new(HashMap::new()),
            transaction: AtomicU32::new(0),
            connections: tokio::sync::Mutex::new(()),
        })
    }
    fn rotator(&self, number: usize) -> Result<Arc<crate::rotator::Rotator>> {
        let slot = self
            .profiles
            .rotators
            .get(number)
            .ok_or_else(|| error(0x401, "Unknown rotator slot"))?;
        Ok(self
            .rotators
            .lock()
            .unwrap()
            .entry(number)
            .or_insert_with(|| {
                Arc::new(crate::rotator::Rotator::new(
                    slot.kind,
                    number,
                    slot.unique_id.clone(),
                    self.profiles.rotators.profile_path(&slot),
                    self.runtime.directory.clone(),
                    self.runtime.simulate,
                ))
            })
            .clone())
    }
    async fn check_rotator_selection(
        &self,
        number: usize,
        serial: Option<&str>,
        active_only: bool,
    ) -> Result<()> {
        let target = self
            .profiles
            .rotators
            .get(number)
            .ok_or_else(|| error(0x401, "Unknown rotator slot"))?;
        if let Some(serial) = serial {
            for slot in self.profiles.rotators.all() {
                if slot.number == number || slot.kind != target.kind {
                    continue;
                }
                let other = self.rotator(slot.number)?.setup().await?;
                if (!active_only || other["connected"] == true)
                    && other["profile"]["serial"]
                        .as_str()
                        .is_some_and(|v| v.eq_ignore_ascii_case(serial))
                {
                    return Err(error(
                        0x40b,
                        format!("Device is already selected by rotator {}", slot.number),
                    ));
                }
            }
        }
        Ok(())
    }
    fn focuser(&self, number: usize) -> Result<Arc<crate::accessory::Accessory>> {
        let slot = self
            .profiles
            .focusers
            .get(number)
            .ok_or_else(|| error(0x401, "Unknown focuser slot"))?;
        Ok(self
            .focusers
            .lock()
            .unwrap()
            .entry(number)
            .or_insert_with(|| {
                Arc::new(crate::accessory::Accessory::new(
                    slot.kind.worker(),
                    number,
                    Some(slot.unique_id.clone()),
                    self.profiles.focusers.profile_path(&slot),
                    self.runtime.directory.clone(),
                    self.runtime.simulate,
                ))
            })
            .clone())
    }
    async fn check_focuser_selection(
        &self,
        number: usize,
        serial: Option<&str>,
        active_only: bool,
    ) -> Result<()> {
        let target = self
            .profiles
            .focusers
            .get(number)
            .ok_or_else(|| error(0x401, "Unknown focuser slot"))?;
        if let Some(serial) = serial {
            for slot in self.profiles.focusers.all() {
                if slot.number == number || slot.kind != target.kind {
                    continue;
                }
                let other = self.focuser(slot.number)?.setup().await?;
                if (!active_only || other["connected"] == true)
                    && other["profile"]["serial"]
                        .as_str()
                        .is_some_and(|s| s.eq_ignore_ascii_case(serial))
                {
                    return Err(error(
                        0x40b,
                        format!("Device is already selected by focuser {}", slot.number),
                    ));
                }
            }
        }
        Ok(())
    }
    pub fn device(&self, slot: usize) -> Result<Arc<Device>> {
        self.profiles.get(slot)?;
        Ok(self
            .devices
            .lock()
            .unwrap()
            .entry(slot)
            .or_insert_with(|| {
                Device::new(
                    slot,
                    self.profiles.clone(),
                    self.runtime.clone(),
                    self.log.diagnostic(Some(slot)),
                )
            })
            .clone())
    }
    fn devices(&self) -> Vec<Arc<Device>> {
        self.devices.lock().unwrap().values().cloned().collect()
    }
    fn next(&self) -> u32 {
        self.transaction
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }
    pub async fn shutdown(&self) {
        if let Some(hub) = &self.hub {
            hub.close();
        }
        self.flatpanel.shutdown().await;
        let rotators: Vec<_> = self.rotators.lock().unwrap().values().cloned().collect();
        for rotator in rotators {
            rotator.shutdown().await;
        }
        self.filterwheel.shutdown().await;
        let focusers: Vec<_> = self.focusers.lock().unwrap().values().cloned().collect();
        for focuser in focusers {
            focuser.shutdown().await;
        }
        for device in self.devices() {
            device.shutdown().await;
        }
    }
    pub async fn poll(self: Arc<Self>, stop: CancellationToken) {
        loop {
            tokio::select! {_=stop.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(2))=>{futures_util::future::join_all(self.devices().iter().map(|camera|camera.refresh())).await;}}
        }
    }
    fn state(&self) -> Value {
        json!({"simulation":self.runtime.simulate,"version":env!("CARGO_PKG_VERSION"),"cameras":self.profiles.all().into_iter().enumerate().map(|(slot,profile)|json!({"slot":slot,"profile":profile,"connected":self.device(slot).is_ok_and(|d|d.in_use())})).collect::<Vec<_>>(),"logs":self.log.events()})
    }
    pub(crate) async fn hub_devices(&self) -> Result<Vec<regain_hub::runtime::OutputDescriptor>> {
        let Some(hub) = &self.hub else {
            return Ok(Vec::new());
        };
        let devices = hub.devices().await?;
        for device in &devices {
            let conflict = match device.device_type {
                regain_hub::config::DeviceType::Camera => {
                    self.profiles.get(device.number as usize).is_ok()
                }
                regain_hub::config::DeviceType::Focuser => {
                    self.profiles.focusers.get(device.number as usize).is_some()
                }
                regain_hub::config::DeviceType::Rotator => {
                    self.profiles.rotators.get(device.number as usize).is_some()
                }
                regain_hub::config::DeviceType::FilterWheel => {
                    device.number == 0 && self.filterwheel.configured().await?.is_some()
                }
                regain_hub::config::DeviceType::CoverCalibrator => {
                    device.number == 0 && self.flatpanel.configured().await?.is_some()
                }
                _ => false,
            };
            anyhow::ensure!(
                !conflict,
                error(
                    0x401,
                    format!(
                        "Hub {} number conflicts with a local slot; choose distinct device numbers",
                        crate::hub_output::class_name(device.device_type).to_lowercase()
                    )
                )
            );
        }
        Ok(devices)
    }
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .merge(crate::hub_setup::routes())
            .route(
                "/",
                get(|| async { axum::response::Redirect::temporary("/setup") }),
            )
            .route(
                "/setup",
                get(|| async { axum::response::Html(include_str!("../web/index.html")) }),
            )
            .route("/setup/v1/camera/{slot}/setup", get(camera_page))
            .route(
                "/style.css",
                get(|| async {
                    (
                        [("Content-Type", "text/css")],
                        include_str!("../web/style.css"),
                    )
                }),
            )
            .route(
                "/setup.js",
                get(|| async {
                    (
                        [("Content-Type", "application/javascript")],
                        include_str!("../web/setup.js"),
                    )
                }),
            )
            .route(
                "/regain.png",
                get(|| async {
                    (
                        [("Content-Type", "image/png")],
                        include_bytes!("../web/regain.png").as_slice(),
                    )
                }),
            )
            .route("/management/apiversions", get(management_versions))
            .route("/management/v1/{member}", get(management))
            .route(
                "/api/v1/camera/{slot}/{member}",
                get(camera_get).put(camera_put),
            )
            .route(
                "/setup/api/state",
                get(|State(s): State<Arc<Server>>| async move { Json(s.state()) }),
            )
            .route("/setup/api/slots", post(add_slot))
            .route("/setup/api/cameras/{slot}", post(configure))
            .route("/setup/api/discover", post(discover))
            .route(
                "/api/v1/rotator/{slot}/{member}",
                get(rotator_get).put(rotator_put),
            )
            .route("/setup/v1/rotator/{slot}/setup", get(rotator_page))
            .route(
                "/rotator.js",
                get(|| async {
                    (
                        [("Content-Type", "application/javascript")],
                        include_str!("../web/rotator.js"),
                    )
                }),
            )
            .route(
                "/setup/api/rotator",
                get(rotator_setup).post(rotator_select),
            )
            .route("/setup/api/rotator/discover", post(rotator_discover))
            .route(
                "/setup/rotators",
                get(|| async { axum::response::Html(include_str!("../web/rotators.html")) }),
            )
            .route(
                "/rotators.js",
                get(|| async {
                    (
                        [(axum::http::header::CONTENT_TYPE, "text/javascript")],
                        include_str!("../web/rotators.js"),
                    )
                }),
            )
            .route("/setup/api/rotators", get(rotators_setup).post(rotator_add))
            .route(
                "/setup/api/rotators/{slot}",
                get(rotator_setup_slot).post(rotator_select_slot),
            )
            .route(
                "/setup/api/rotators/{slot}/discover",
                post(rotator_discover_slot),
            )
            .route(
                "/api/v1/{accessory}/{slot}/{member}",
                get(accessory_get).put(accessory_put),
            )
            .route("/setup/v1/filterwheel/{slot}/setup", get(filterwheel_page))
            .route("/setup/v1/focuser/{slot}/setup", get(focuser_page))
            .route(
                "/setup/focusers",
                get(|| async { axum::response::Html(include_str!("../web/focusers.html")) }),
            )
            .route(
                "/focusers.js",
                get(|| async {
                    (
                        [("Content-Type", "application/javascript")],
                        include_str!("../web/focusers.js"),
                    )
                }),
            )
            .route("/setup/api/focusers", get(focuser_list).post(focuser_add))
            .route(
                "/setup/api/focusers/{slot}",
                get(focuser_setup).post(focuser_configure),
            )
            .route(
                "/setup/api/focusers/{slot}/discover",
                post(focuser_discover),
            )
            .route(
                "/setup/api/focusers/{slot}/settings",
                post(focuser_settings),
            )
            .route(
                "/setup/v1/covercalibrator/{slot}/setup",
                get(covercalibrator_page),
            )
            .route(
                "/flatpanel.js",
                get(|| async {
                    (
                        [("Content-Type", "application/javascript")],
                        include_str!("../web/flatpanel.js"),
                    )
                }),
            )
            .route(
                "/setup/api/flatpanel",
                get(flatpanel_setup).post(flatpanel_configure),
            )
            .route("/setup/api/flatpanel/discover", post(flatpanel_discover))
            .route(
                "/accessory.js",
                get(|| async {
                    (
                        [("Content-Type", "application/javascript")],
                        include_str!("../web/accessory.js"),
                    )
                }),
            )
            .route(
                "/setup/api/accessory/{kind}",
                get(accessory_setup).post(accessory_configure),
            )
            .route(
                "/setup/api/accessory/{kind}/discover",
                post(accessory_discover),
            )
            .route(
                "/setup/api/accessory/{kind}/settings",
                post(accessory_settings),
            )
            .layer(DefaultBodyLimit::max(65536))
            .with_state(self.clone())
    }
}
fn envelope(value: Value, client: u32, server: u32) -> Value {
    json!({"Value":value,"ClientTransactionID":client,"ServerTransactionID":server,"ErrorNumber":0,"ErrorMessage":""})
}
pub(crate) fn error_code(e: &anyhow::Error) -> i32 {
    if let Some(e) = e.downcast_ref::<Error>() {
        e.0
    } else if matches!(e.downcast_ref::<Failure>(), Some(Failure::Invalid(_)))
        || e.is::<serde_json::Error>()
    {
        0x401
    } else {
        0x500
    }
}
fn failure(e: anyhow::Error, client: u32, server: u32) -> Value {
    json!({"ClientTransactionID":client,"ServerTransactionID":server,"ErrorNumber":error_code(&e),"ErrorMessage":format!("{e:#}")})
}
async fn management_versions(State(s): State<Arc<Server>>, RawQuery(q): RawQuery) -> Json<Value> {
    let p = Params::parse(q.as_deref().unwrap_or(""));
    Json(match p.and_then(|p| p.optional_id("ClientTransactionID")) {
        Ok(id) => envelope(json!([1]), id, s.next()),
        Err(e) => failure(e, 0, s.next()),
    })
}
async fn management(
    State(s): State<Arc<Server>>,
    Path(member): Path<String>,
    RawQuery(q): RawQuery,
) -> Response {
    let id = Params::parse(q.as_deref().unwrap_or(""))
        .and_then(|p| p.optional_id("ClientTransactionID"))
        .unwrap_or(0);
    let value = match member.as_str() {
        "description" => {
            json!({"ServerName":"PulsarFab regain","Manufacturer":"PulsarFab","ManufacturerVersion":env!("CARGO_PKG_VERSION"),"Location":"Astronomy equipment server"})
        }
        "configureddevices" => {
            let mut devices = s.profiles.all().into_iter().enumerate().filter(|(_,p)|p.camera.is_some()).map(|(slot,p)|json!({"DeviceName":p.label,"DeviceType":"Camera","DeviceNumber":slot,"UniqueID":p.unique_id})).collect::<Vec<_>>();
            for slot in s.profiles.rotators.all() {
                match match s.rotator(slot.number) {
                    Ok(r) => r.configured().await,
                    Err(e) => Err(e),
                } {
                    Ok(Some(r)) => devices.push(r),
                    Ok(None) => (),
                    Err(e) => return Json(failure(e, id, s.next())).into_response(),
                };
            }
            let mut accessories = vec![s.filterwheel.configured().await];
            for slot in s.profiles.focusers.all() {
                accessories.push(match s.focuser(slot.number) {
                    Ok(f) => f.configured().await,
                    Err(e) => Err(e),
                });
            }
            for accessory in accessories {
                match accessory {
                    Ok(Some(d)) => devices.push(d),
                    Ok(None) => (),
                    Err(e) => return Json(failure(e, id, s.next())).into_response(),
                }
            }
            match s.flatpanel.configured().await {
                Ok(Some(d)) => devices.push(d),
                Ok(None) => (),
                Err(e) => return Json(failure(e, id, s.next())).into_response(),
            }
            if s.hub.is_some() {
                match s.hub_devices().await {
                    Ok(outputs) => {
                        devices.extend(outputs.iter().map(crate::hub_output::configured_device))
                    }
                    Err(e) => return Json(failure(e, id, s.next())).into_response(),
                }
            }
            json!(devices)
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    Json(envelope(value, id, s.next())).into_response()
}
async fn hub_request(
    s: Arc<Server>,
    kind: String,
    slot: u32,
    member: String,
    put: bool,
    params: Result<Params>,
) -> Response {
    let Some(hub) = &s.hub else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !matches!(
        kind.as_str(),
        "switch"
            | "safetymonitor"
            | "observingconditions"
            | "focuser"
            | "rotator"
            | "filterwheel"
            | "covercalibrator"
    ) || member != member.to_lowercase()
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let server = s.next();
    let mut transaction = 0;
    let result = async {
        let params = params?;
        transaction = params.optional_id("ClientTransactionID")?;
        params.optional_id("ClientID")?;
        let device = s.hub_devices().await?.into_iter().find(|d| {
            d.number == slot && crate::hub_output::class_name(d.device_type).to_lowercase() == kind
        });
        let Some(device) = device else {
            return Ok(StatusCode::NOT_FOUND.into_response());
        };
        Ok(Json(envelope(
            hub.request(&device, &member, put, &params).await?,
            transaction,
            server,
        ))
        .into_response())
    }
    .await;
    match result {
        Ok(response) => response,
        Err(e) => Json(failure(e, transaction, server)).into_response(),
    }
}
async fn camera_get(
    State(s): State<Arc<Server>>,
    Path((slot, member)): Path<(usize, String)>,
    RawQuery(q): RawQuery,
    headers: HeaderMap,
) -> Response {
    camera(
        s,
        slot,
        member,
        Method::GET,
        Params::parse(q.as_deref().unwrap_or("")),
        headers,
    )
    .await
}
async fn camera_put(
    State(s): State<Arc<Server>>,
    Path((slot, member)): Path<(usize, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    camera(s, slot, member, Method::PUT, Params::parse(&body), headers).await
}
async fn camera(
    s: Arc<Server>,
    slot: usize,
    member: String,
    method: Method,
    params: Result<Params>,
    headers: HeaderMap,
) -> Response {
    if member != member.to_lowercase() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if s.hub.is_some() {
        match s.hub_devices().await {
            Ok(devices) => {
                if let Some(device) = devices.into_iter().find(|device| {
                    device.device_type == regain_hub::config::DeviceType::Camera
                        && device.number as usize == slot
                }) {
                    return hub_camera(s, device, member, method == Method::PUT, params, headers)
                        .await;
                }
            }
            Err(failure) => {
                let transaction = params
                    .as_ref()
                    .ok()
                    .and_then(|p| p.optional_id("ClientTransactionID").ok())
                    .unwrap_or(0);
                let server = s.next();
                if method == Method::GET
                    && matches!(member.as_str(), "imagearray" | "imagearrayvariant")
                    && accepts_imagebytes(&headers)
                {
                    return image_error(
                        error_code(&failure),
                        &format!("{failure:#}"),
                        transaction,
                        server,
                    );
                }
                return Json(self::failure(failure, transaction, server)).into_response();
            }
        }
    }
    let Ok(device) = s.device(slot) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let server = s.next();
    let mut client_transaction = 0;
    let image =
        method == Method::GET && matches!(member.as_str(), "imagearray" | "imagearrayvariant");
    let binary = accepts_imagebytes(&headers);
    let result=async {
        let p=params?;let client=p.optional_id("ClientID")?;client_transaction=p.optional_id("ClientTransactionID")?;
        if image{return Ok(image_response(device.image(client)?,binary,client_transaction,server))}
        let value=if method==Method::GET{device.get(&member,client)?}else if matches!(member.as_str(),"connected"|"connect"|"disconnect"){
            let _gate=s.connections.lock().await;let on=if member=="connected"{p.boolean("Connected")?}else{member=="connect"};
            if on&&!device.in_use(){let profile=s.profiles.get(slot)?;for other in s.devices(){if other.slot!=slot&&other.in_use(){let p=s.profiles.get(other.slot)?;if p.camera.as_ref().map(|v|&v["name"])==profile.camera.as_ref().map(|v|&v["name"])&&(p.serial.is_none()||profile.serial.is_none()||p.serial==profile.serial){return Err(error(0x40B,"This camera is already in use by another slot; identical models need distinct serial numbers"))}}}}
            device.connected(client,on,member!="connected").await?;Value::Null
        }else if member=="abortexposure" {device.abort(client).await?;Value::Null}else{device.put(&member,&p,client)?};
        Ok(Json(envelope(value,client_transaction,server)).into_response())
    }.await;
    match result {
        Ok(response) => response,
        Err(e) => {
            (s.log.diagnostic(Some(slot)))("warning", "ascom.failed", &format!("{member}: {e:#}"));
            if image && binary {
                image_error(
                    error_code(&e),
                    &format!("{e:#}"),
                    client_transaction,
                    server,
                )
            } else {
                Json(failure(e, client_transaction, server)).into_response()
            }
        }
    }
}
fn accepts_imagebytes(headers: &HeaderMap) -> bool {
    headers
        .get("accept")
        .and_then(|s| s.to_str().ok())
        .is_some_and(|s| {
            s.split(',').any(|m| {
                let mut parts = m.trim().split(';');
                if !parts
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/imagebytes"))
                {
                    return false;
                }
                let mut quality = None;
                for parameter in parts {
                    if let Some((name, value)) = parameter.trim().split_once('=')
                        && name.trim().eq_ignore_ascii_case("q")
                    {
                        if quality.is_some() {
                            return false;
                        }
                        let Ok(value) = value.trim().parse::<f64>() else {
                            return false;
                        };
                        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                            return false;
                        }
                        quality = Some(value);
                    }
                }
                quality.unwrap_or(1.0) > 0.0
            })
        })
}
async fn hub_camera(
    s: Arc<Server>,
    device: regain_hub::runtime::OutputDescriptor,
    member: String,
    put: bool,
    params: Result<Params>,
    headers: HeaderMap,
) -> Response {
    let server = s.next();
    let mut transaction = 0;
    let image = !put && matches!(member.as_str(), "imagearray" | "imagearrayvariant");
    let binary = accepts_imagebytes(&headers);
    let result = async {
        let params = params?;
        transaction = params.optional_id("ClientTransactionID")?;
        let hub = s.hub.as_ref().unwrap();
        if image {
            return hub
                .image(&device, &params, binary, transaction, server)
                .await;
        }
        Ok(Json(envelope(
            hub.request(&device, &member, put, &params).await?,
            transaction,
            server,
        ))
        .into_response())
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) if image && binary => image_error(
            error_code(&error),
            &format!("{error:#}"),
            transaction,
            server,
        ),
        Err(error) => Json(failure(error, transaction, server)).into_response(),
    }
}
pub fn image_header(width: u32, height: u32, client: u32, server: u32, error: u32) -> Vec<u8> {
    [1, error, client, server, 44, 2, 8, 2, width, height, 0]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}
fn image_error(error: i32, message: &str, client: u32, server: u32) -> Response {
    let mut bytes = image_header(0, 0, client, server, error as u32);
    bytes.extend_from_slice(message.as_bytes());
    (
        [
            ("Content-Type", "application/imagebytes"),
            ("Cache-Control", "no-store"),
        ],
        bytes,
    )
        .into_response()
}
pub fn image_response(frame: Arc<Frame>, binary: bool, client: u32, server: u32) -> Response {
    let width = frame.exposure.width as usize;
    let height = frame.exposure.height as usize;
    let count = width * height;
    let prefix = if binary {
        image_header(width as u32, height as u32, client, server, 0)
    } else {
        format!("{{\"ClientTransactionID\":{client},\"ServerTransactionID\":{server},\"ErrorNumber\":0,\"ErrorMessage\":\"\",\"Type\":2,\"Rank\":2,\"Value\":[").into_bytes()
    };
    let chunks = stream::unfold(
        (frame, 0usize, Some(prefix)),
        move |(frame, mut i, prefix)| async move {
            if let Some(prefix) = prefix {
                return Some((Ok::<_, Infallible>(Bytes::from(prefix)), (frame, i, None)));
            }
            if i > count {
                return None;
            }
            let mut bytes = Vec::with_capacity(65536);
            if i == count {
                if binary {
                    return None;
                }
                bytes.extend_from_slice(b"]}");
                return Some((Ok(Bytes::from(bytes)), (frame, count + 1, None)));
            }
            while i < count && bytes.len() < 65000 {
                let x = i / height;
                let y = i % height;
                let source = (y * width + x) * 2;
                if binary {
                    bytes.extend_from_slice(&frame.pixels[source..source + 2]);
                } else {
                    if y == 0 {
                        if x > 0 {
                            bytes.push(b',')
                        }
                        bytes.push(b'[');
                    } else {
                        bytes.push(b',')
                    };
                    let value =
                        u16::from_le_bytes([frame.pixels[source], frame.pixels[source + 1]]);
                    bytes.extend_from_slice(value.to_string().as_bytes());
                    if y + 1 == height {
                        bytes.push(b']');
                    }
                }
                i += 1;
            }
            Some((Ok(Bytes::from(bytes)), (frame, i, None)))
        },
    );
    let mut response = Body::from_stream(chunks).into_response();
    response.headers_mut().insert(
        "Content-Type",
        if binary {
            "application/imagebytes"
        } else {
            "application/json"
        }
        .parse()
        .unwrap(),
    );
    response
        .headers_mut()
        .insert("Cache-Control", "no-store".parse().unwrap());
    if binary {
        response.headers_mut().insert(
            "Content-Length",
            (44 + count * 2).to_string().parse().unwrap(),
        );
    }
    response
}
pub(crate) fn setup_allowed(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
        && headers.get("origin").is_none_or(|origin| {
            headers
                .get("host")
                .and_then(|h| h.to_str().ok())
                .is_some_and(|host| origin.to_str().ok() == Some(&format!("http://{host}")))
        })
}
fn setup_result(result: Result<Value>) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":format!("{e:#}")})),
        )
            .into_response(),
    }
}
async fn add_slot(State(s): State<Arc<Server>>, headers: HeaderMap) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    setup_result(s.profiles.add().map(|slot| json!({"slot":slot})))
}
async fn configure(
    State(s): State<Arc<Server>>,
    Path(slot): Path<usize>,
    headers: HeaderMap,
    Json(p): Json<Profile>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    setup_result(
        s.device(slot)
            .and_then(|d| d.configure(p))
            .map(|_| Value::Null),
    )
}
async fn discover(
    State(s): State<Arc<Server>>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let _gate = s.connections.lock().await;
    if s.devices().iter().any(|d| d.in_use()) {
        return setup_result(Err(error(0x40B, "Disconnect cameras before scanning USB")));
    }
    setup_result(
        s.runtime
            .list(
                value["direct"] == true,
                s.log.diagnostic(None),
                &CancellationToken::new(),
            )
            .await,
    )
}
async fn rotator_get(
    State(s): State<Arc<Server>>,
    Path((slot, member)): Path<(usize, String)>,
    RawQuery(q): RawQuery,
) -> Response {
    rotator_request(
        s,
        slot,
        member,
        false,
        Params::parse(q.as_deref().unwrap_or("")),
    )
    .await
}
async fn rotator_put(
    State(s): State<Arc<Server>>,
    Path((slot, member)): Path<(usize, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    rotator_request(s, slot, member, true, Params::parse(&body)).await
}
async fn rotator_request(
    s: Arc<Server>,
    slot: usize,
    member: String,
    put: bool,
    params: Result<Params>,
) -> Response {
    if s.hub.is_some() {
        match s.hub_devices().await {
            Ok(devices)
                if devices.iter().any(|device| {
                    device.device_type == regain_hub::config::DeviceType::Rotator
                        && device.number as usize == slot
                }) =>
            {
                return hub_request(s, "rotator".into(), slot as u32, member, put, params).await;
            }
            Ok(_) => (),
            Err(e) => {
                let transaction = params
                    .as_ref()
                    .ok()
                    .and_then(|p| p.optional_id("ClientTransactionID").ok())
                    .unwrap_or(0);
                return Json(failure(e, transaction, s.next())).into_response();
            }
        }
    }
    if s.profiles.rotators.get(slot).is_none() || member != member.to_lowercase() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut id = 0;
    let result = async {
        let p = params?;
        id = p.optional_id("ClientTransactionID")?;
        let _gate = s.connections.lock().await;
        let rotator = s.rotator(slot)?;
        if member == "connected" && put && p.boolean("Connected")? {
            let state = rotator.setup().await?;
            s.check_rotator_selection(slot, state["profile"]["serial"].as_str(), true)
                .await?;
        }
        rotator.request(&member, put, &p).await
    }
    .await;
    Json(match result {
        Ok(v) => envelope(v, id, s.next()),
        Err(e) => failure(e, id, s.next()),
    })
    .into_response()
}
async fn rotator_page(State(s): State<Arc<Server>>, Path(slot): Path<usize>) -> Response {
    if let Some(page) = hub_device_page(&s, regain_hub::config::DeviceType::Rotator, slot).await {
        return page;
    }
    if s.profiles.rotators.get(slot).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    axum::response::Html(include_str!("../web/rotator.html")).into_response()
}
async fn rotators_setup(State(s): State<Arc<Server>>) -> Response {
    setup_result(
        async {
            let mut rows = vec![];
            for slot in s.profiles.rotators.all() {
                rows.push(s.rotator(slot.number)?.setup().await?);
            }
            Ok(json!(rows))
        }
        .await,
    )
}
async fn rotator_add(
    State(s): State<Arc<Server>>,
    headers: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    setup_result((|| {
        Ok(json!({"slot":s.profiles.rotators.add(serde_json::from_value(v["kind"].clone())?)?}))
    })())
}
async fn rotator_setup(State(s): State<Arc<Server>>) -> Response {
    rotator_setup_slot(State(s), Path(0)).await
}
async fn rotator_setup_slot(State(s): State<Arc<Server>>, Path(slot): Path<usize>) -> Response {
    setup_result(async { s.rotator(slot)?.setup().await }.await)
}
async fn rotator_select(State(s): State<Arc<Server>>, h: HeaderMap, j: Json<Value>) -> Response {
    rotator_select_slot(State(s), Path(0), h, j).await
}
async fn rotator_select_slot(
    State(s): State<Arc<Server>>,
    Path(slot): Path<usize>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let serial = match &value["serial"] {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        _ => return setup_result(Err(error(0x401, "Invalid serial"))),
    };
    let _gate = s.connections.lock().await;
    setup_result(
        async {
            s.check_rotator_selection(slot, serial.as_deref(), false)
                .await?;
            s.rotator(slot)?.select(serial).await
        }
        .await,
    )
}
async fn rotator_discover(State(s): State<Arc<Server>>, h: HeaderMap) -> Response {
    rotator_discover_slot(State(s), Path(0), h).await
}
async fn rotator_discover_slot(
    State(s): State<Arc<Server>>,
    Path(slot): Path<usize>,
    headers: HeaderMap,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let _gate = s.connections.lock().await;
    setup_result(
        async {
            let target = s
                .profiles
                .rotators
                .get(slot)
                .ok_or_else(|| error(0x401, "Unknown rotator slot"))?;
            for other in s.profiles.rotators.all() {
                if other.kind == target.kind
                    && s.rotator(other.number)?.setup().await?["connected"] == true
                {
                    return Err(error(
                        0x40b,
                        "Disconnect rotators of this model before scanning",
                    ));
                }
            }
            s.rotator(slot)?.discover().await
        }
        .await,
    )
}

async fn accessory_page() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../web/accessory.html"))
}
async fn camera_page(State(s): State<Arc<Server>>, Path(slot): Path<usize>) -> Response {
    if let Some(page) = hub_device_page(&s, regain_hub::config::DeviceType::Camera, slot).await {
        return page;
    }
    axum::response::Html(include_str!("../web/index.html")).into_response()
}
async fn hub_device_page(
    s: &Server,
    kind: regain_hub::config::DeviceType,
    slot: usize,
) -> Option<Response> {
    s.hub.as_ref()?;
    match s.hub_devices().await {
        Ok(devices)
            if devices
                .iter()
                .any(|device| device.device_type == kind && device.number as usize == slot) =>
        {
            Some(axum::response::Html(include_str!("../web/hub.html")).into_response())
        }
        Ok(_) => None,
        Err(_) => Some(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    }
}
async fn filterwheel_page(State(s): State<Arc<Server>>, Path(slot): Path<usize>) -> Response {
    if let Some(page) = hub_device_page(&s, regain_hub::config::DeviceType::FilterWheel, slot).await
    {
        return page;
    }
    if slot != 0 {
        return StatusCode::NOT_FOUND.into_response();
    }
    accessory_page().await.into_response()
}
async fn covercalibrator_page(State(s): State<Arc<Server>>, Path(slot): Path<usize>) -> Response {
    if let Some(page) =
        hub_device_page(&s, regain_hub::config::DeviceType::CoverCalibrator, slot).await
    {
        return page;
    }
    if slot != 0 {
        return StatusCode::NOT_FOUND.into_response();
    }
    axum::response::Html(include_str!("../web/flatpanel.html")).into_response()
}
async fn accessory_get(
    State(s): State<Arc<Server>>,
    Path((kind, slot, member)): Path<(String, usize, String)>,
    RawQuery(q): RawQuery,
) -> Response {
    accessory_request(
        s,
        kind,
        slot,
        member,
        false,
        Params::parse(q.as_deref().unwrap_or("")),
    )
    .await
}
async fn accessory_put(
    State(s): State<Arc<Server>>,
    Path((kind, slot, member)): Path<(String, usize, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"))
    {
        return StatusCode::BAD_REQUEST.into_response();
    }
    accessory_request(s, kind, slot, member, true, Params::parse(&body)).await
}
async fn accessory_request(
    s: Arc<Server>,
    kind: String,
    slot: usize,
    member: String,
    put: bool,
    params: Result<Params>,
) -> Response {
    if matches!(kind.as_str(), "focuser" | "filterwheel" | "covercalibrator") && s.hub.is_some() {
        match s.hub_devices().await {
            Ok(devices)
                if devices.iter().any(|device| {
                    crate::hub_output::class_name(device.device_type).to_lowercase() == kind
                        && device.number as usize == slot
                }) =>
            {
                return hub_request(s, kind, slot as u32, member, put, params).await;
            }
            Ok(_) => (),
            Err(e) => {
                let transaction = params
                    .as_ref()
                    .ok()
                    .and_then(|p| p.optional_id("ClientTransactionID").ok())
                    .unwrap_or(0);
                return Json(failure(e, transaction, s.next())).into_response();
            }
        }
    }
    if matches!(
        kind.as_str(),
        "switch" | "safetymonitor" | "observingconditions"
    ) {
        let Ok(slot) = u32::try_from(slot) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        return hub_request(s, kind, slot, member, put, params).await;
    }
    if !((slot == 0 && kind != "focuser")
        || (kind == "focuser" && s.profiles.focusers.get(slot).is_some()))
        || member != member.to_lowercase()
        || !["filterwheel", "focuser", "covercalibrator"].contains(&kind.as_str())
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut id = 0;
    let result = async {
        let p = params?;
        id = p.optional_id("ClientTransactionID")?;
        if kind == "covercalibrator" {
            s.flatpanel.request(&member, put, &p).await
        } else if kind == "focuser" {
            let target = s.focuser(slot)?;
            let _gate = if put && matches!(member.as_str(), "connected" | "link") {
                Some(s.connections.lock().await)
            } else {
                None
            };
            if put
                && matches!(member.as_str(), "connected" | "link")
                && p.boolean(if member == "link" {
                    "Link"
                } else {
                    "Connected"
                })?
            {
                let state = target.setup().await?;
                s.check_focuser_selection(slot, state["profile"]["serial"].as_str(), true)
                    .await?;
            }
            target.request(&member, put, &p).await
        } else {
            s.filterwheel.request(&member, put, &p).await
        }
    }
    .await;
    Json(match result {
        Ok(v) => envelope(v, id, s.next()),
        Err(e) => failure(e, id, s.next()),
    })
    .into_response()
}
async fn accessory_setup(State(s): State<Arc<Server>>, Path(kind): Path<String>) -> Response {
    if !["efw", "filterwheel"].contains(&kind.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    setup_result(s.filterwheel.setup().await)
}
async fn flatpanel_setup(State(s): State<Arc<Server>>) -> Response {
    setup_result(s.flatpanel.setup().await)
}
async fn flatpanel_configure(
    State(s): State<Arc<Server>>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    setup_result(s.flatpanel.configure(value).await)
}
async fn flatpanel_discover(State(s): State<Arc<Server>>, headers: HeaderMap) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    setup_result(s.flatpanel.discover().await)
}
async fn accessory_configure(
    State(s): State<Arc<Server>>,
    Path(kind): Path<String>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !["efw", "filterwheel"].contains(&kind.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    setup_result(s.filterwheel.configure(value).await)
}
async fn accessory_discover(
    State(s): State<Arc<Server>>,
    Path(kind): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !["efw", "filterwheel"].contains(&kind.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    setup_result(s.filterwheel.discover().await)
}
async fn accessory_settings(
    State(s): State<Arc<Server>>,
    Path(kind): Path<String>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !["efw", "filterwheel"].contains(&kind.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    setup_result(s.filterwheel.settings(value).await)
}

async fn focuser_page(State(s): State<Arc<Server>>, Path(slot): Path<usize>) -> Response {
    if let Some(page) = hub_device_page(&s, regain_hub::config::DeviceType::Focuser, slot).await {
        return page;
    }
    if s.profiles.focusers.get(slot).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    accessory_page().await.into_response()
}
async fn focuser_list(State(s): State<Arc<Server>>) -> Response {
    setup_result(
        async {
            let mut rows = Vec::new();
            for slot in s.profiles.focusers.all() {
                rows.push(s.focuser(slot.number)?.setup().await?);
            }
            Ok(json!(rows))
        }
        .await,
    )
}
async fn focuser_add(
    State(s): State<Arc<Server>>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    setup_result((|| {
        let kind = serde_json::from_value(value["kind"].clone())?;
        Ok(json!({"slot":s.profiles.focusers.add(kind)?}))
    })())
}
async fn focuser_setup(State(s): State<Arc<Server>>, Path(slot): Path<usize>) -> Response {
    setup_result(async { s.focuser(slot)?.setup().await }.await)
}
async fn focuser_configure(
    State(s): State<Arc<Server>>,
    Path(slot): Path<usize>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let _gate = s.connections.lock().await;
    setup_result(
        async {
            s.check_focuser_selection(slot, value["serial"].as_str(), false)
                .await?;
            s.focuser(slot)?.configure(value).await
        }
        .await,
    )
}
async fn focuser_discover(
    State(s): State<Arc<Server>>,
    Path(slot): Path<usize>,
    headers: HeaderMap,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let _gate = s.connections.lock().await;
    setup_result(
        async {
            let target = s
                .profiles
                .focusers
                .get(slot)
                .ok_or_else(|| error(0x401, "Unknown focuser slot"))?;
            for other in s.profiles.focusers.all() {
                if other.kind == target.kind
                    && s.focuser(other.number)?.setup().await?["connected"] == true
                {
                    return Err(error(
                        0x40b,
                        "Disconnect focusers of this model before scanning",
                    ));
                }
            }
            s.focuser(slot)?.discover().await
        }
        .await,
    )
}
async fn focuser_settings(
    State(s): State<Arc<Server>>,
    Path(slot): Path<usize>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Response {
    if !setup_allowed(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    setup_result(async { s.focuser(slot)?.settings(value).await }.await)
}

pub async fn discovery(address: Ipv4Addr, port: u16, token: CancellationToken) -> Result<()> {
    let socket = UdpSocket::bind(SocketAddr::from((address, 32227))).await?;
    let reply = serde_json::to_vec(&json!({"AlpacaPort":port}))?;
    let mut bytes = [0; 256];
    loop {
        tokio::select! {_=token.cancelled()=>return Ok(()),result=socket.recv_from(&mut bytes)=>{let(n,peer)=result?;if &bytes[..n]==b"alpacadiscovery1"{socket.send_to(&reply,peer).await?;}}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[test]
    fn imagebytes_negotiation_respects_quality_zero_case_and_malformed_values() {
        for (value, expected) in [
            ("application/imagebytes", true),
            ("application/json, Application/ImageBytes; Q = 0.25", true),
            ("application/imagebytes;q=0", false),
            ("application/imagebytes;q=0.000, application/json", false),
            ("application/imagebytes;q=NaN", false),
            ("application/imagebytes;q=2", false),
            ("application/imagebytes;q=invalid", false),
            ("application/imagebytes;q=0;q=1", false),
            ("application/json", false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("Accept", value.parse().unwrap());
            assert_eq!(accepts_imagebytes(&headers), expected, "{value}");
        }
    }
    fn runtime() -> Runtime {
        Runtime {
            directory: std::env::var_os("REGAIN_TEST_WORKERS")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
                }),
            sdk: "unused".into(),
            simulate: true,
            sdk_simulation: None,
        }
    }
    async fn request(router: &Router, method: &str, path: &str, body: &str) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    #[tokio::test]
    async fn imagebytes_and_json_use_xy_order_without_losing_unsigned_pixels() {
        let pixels: Vec<u8> = [1u16, 2, 65535, 4, 50000, 6]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        let frame = Arc::new(Frame {
            exposure: regain_core::Exposure {
                width: 3,
                height: 2,
                bin: 1,
                x: 0,
                y: 0,
                microseconds: 1000,
                dark: true,
            },
            pixels: pixels.into(),
            metadata: Value::Null,
        });
        let response = image_response(frame.clone(), true, u32::MAX, 42);
        assert_eq!(response.headers()["content-type"], "application/imagebytes");
        let data = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&data[8..12], &u32::MAX.to_le_bytes());
        assert_eq!(&data[20..28], &[2, 0, 0, 0, 8, 0, 0, 0]);
        let values: Vec<u16> = data[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .collect();
        assert_eq!(values, [1, 4, 2, 50000, 65535, 6]);
        let budget = regain_hub::camera::image::ImageBudget::new(12).unwrap();
        let decoded =
            regain_hub::camera::image::read_imagebytes(&mut data.as_ref(), &budget, u32::MAX)
                .await
                .unwrap();
        assert_eq!(decoded.image.descriptor().width(), 3);
        assert_eq!(decoded.image.descriptor().height(), 2);
        assert_eq!(
            decoded.image.descriptor().element_type(),
            regain_hub::camera::image::ElementType::Int32
        );
        assert_eq!(
            decoded.image.descriptor().transmission_type(),
            regain_hub::camera::image::ElementType::UInt16
        );
        assert_eq!(decoded.image.imagebytes_chunk(0, 12).unwrap(), data[44..]);
        drop(decoded);
        assert_eq!(budget.used_bytes(), 0);
        let response = image_response(frame, false, 7, 8);
        let data = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(value["Value"], json!([[1, 4], [2, 50000], [65535, 6]]));
    }
    #[tokio::test]
    async fn sdk_and_direct_camera_contract_over_http() {
        for (direct, fallback) in [(false, false), (true, false), (true, true)] {
            let mut runtime = runtime();
            if fallback {
                runtime.sdk_simulation = Some(
                    json!({"name":"ZWO ASI676MC","width":3552,"height":3552,"bins":[1],"serial":"direct-simulator","cooled":false,"instant":true}),
                );
            }
            let log = Log::new(None);
            let cameras = runtime
                .list(direct, log.diagnostic(None), &CancellationToken::new())
                .await
                .unwrap();
            let profiles = Arc::new(Profiles::new(None).unwrap());
            profiles
                .save(
                    0,
                    Profile {
                        camera: Some(cameras[0].clone()),
                        direct,
                        sdk_fallback: fallback,
                        recovery: regain_core::RecoveryOptions {
                            max_retries: 0,
                            reconnect_delay_seconds: 0.01,
                            ..Default::default()
                        },
                        ..Profile::default()
                    },
                )
                .unwrap();
            let server = Server::new(profiles, runtime, log);
            let router = server.router();
            let (_, value) = request(
                &router,
                "GET",
                "/management/apiversions?CLIENTTRANSACTIONID=25",
                "",
            )
            .await;
            assert_eq!(value["ClientTransactionID"], 25);
            let (_, value) = request(&router, "GET", "/management/v1/configureddevices", "").await;
            assert_eq!(value["Value"][0]["DeviceNumber"], 0);
            assert_eq!(
                request(&router, "GET", "/api/v1/camera/200/name", "")
                    .await
                    .0,
                StatusCode::NOT_FOUND
            );
            let (_, value) = request(
                &router,
                "PUT",
                "/api/v1/camera/0/connected",
                "Connected=true&ClientID=7&ClientTransactionID=20",
            )
            .await;
            assert_eq!(value["ErrorNumber"], 0, "{value}");
            for (member, parameters) in [
                ("numx", if fallback { "NumX=8" } else { "NumX=64" }),
                ("numy", if fallback { "NumY=2" } else { "NumY=64" }),
                ("gain", "Gain=100"),
                ("startexposure", "Duration=0.01&Light=false"),
            ] {
                let (_, value) = request(
                    &router,
                    "PUT",
                    &format!("/api/v1/camera/0/{member}"),
                    &format!("{parameters}&ClientID=7"),
                )
                .await;
                assert_eq!(value["ErrorNumber"], 0, "{value}");
            }
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let (_, value) =
                    request(&router, "GET", "/api/v1/camera/0/imageready?ClientID=7", "").await;
                assert_eq!(value["ErrorNumber"], 0, "{value}");
                if value["Value"] == true {
                    break;
                }
                assert!(tokio::time::Instant::now() < deadline);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let (_, image) =
                request(&router, "GET", "/api/v1/camera/0/imagearray?ClientID=7", "").await;
            assert_eq!(
                image["Value"].as_array().unwrap().len(),
                if fallback { 8 } else { 64 }
            );
            assert_eq!(
                image["Value"][0].as_array().unwrap().len(),
                if fallback { 2 } else { 64 }
            );
            if fallback {
                assert_eq!(server.device(0).unwrap().snapshot().unwrap().backend, "sdk");
            }
            let (_, value) = request(
                &router,
                "PUT",
                "/api/v1/camera/0/connected",
                "Connected=true&ClientID=8",
            )
            .await;
            assert_eq!(value["ErrorNumber"], 0);
            request(
                &router,
                "PUT",
                "/api/v1/camera/0/connected",
                "Connected=false&ClientID=7",
            )
            .await;
            assert_eq!(
                request(&router, "GET", "/api/v1/camera/0/connected?ClientID=8", "")
                    .await
                    .1["Value"],
                true
            );
            assert_eq!(
                request(&router, "GET", "/api/v1/camera/0/imageready?ClientID=7", "")
                    .await
                    .1["ErrorNumber"],
                0x407
            );
            server.shutdown().await;
        }
    }
    #[test]
    fn camera_slots_expand_and_persist_stable_ids() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("cameras.json");
        let profiles = Profiles::new(Some(path.clone())).unwrap();
        let id = profiles.get(0).unwrap().unique_id;
        for slot in 2..10 {
            assert_eq!(profiles.add().unwrap(), slot)
        }
        profiles
            .save(
                0,
                Profile {
                    label: "My camera".into(),
                    ..Profile::default()
                },
            )
            .unwrap();
        let reopened = Profiles::new(Some(path)).unwrap();
        assert_eq!(reopened.all().len(), 10);
        assert_eq!(reopened.get(0).unwrap().unique_id, id);
    }
    #[tokio::test]
    async fn setup_rejects_cross_origin_writes() {
        let server = Server::new(
            Arc::new(Profiles::new(None).unwrap()),
            runtime(),
            Log::new(None),
        );
        let response = server
            .router()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/setup/api/slots")
                    .header("content-type", "application/json")
                    .header("host", "127.0.0.1:11111")
                    .header("origin", "http://unrelated.invalid")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
