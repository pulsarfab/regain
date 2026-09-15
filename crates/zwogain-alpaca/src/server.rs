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
use zwogain_core::{CancellationToken, Diagnostic, Failure, Frame, Runtime};

pub struct Server {
    pub profiles: Arc<Profiles>,
    pub runtime: Runtime,
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
            let _ = writeln!(std::io::stderr().lock(), "ZWOGAIN_DIAGNOSTIC {record}");
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
                        "zwogain-{}.log",
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
        Arc::new(Self {
            profiles,
            runtime,
            log,
            devices: Mutex::new(HashMap::new()),
            transaction: AtomicU32::new(0),
            connections: tokio::sync::Mutex::new(()),
        })
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
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route(
                "/",
                get(|| async { axum::response::Redirect::temporary("/setup") }),
            )
            .route(
                "/setup",
                get(|| async { axum::response::Html(include_str!("../web/index.html")) }),
            )
            .route(
                "/setup/v1/camera/{slot}/setup",
                get(|| async { axum::response::Html(include_str!("../web/index.html")) }),
            )
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
                "/zwogain.png",
                get(|| async {
                    (
                        [("Content-Type", "image/png")],
                        include_bytes!("../web/zwogain.png").as_slice(),
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
            .layer(DefaultBodyLimit::max(65536))
            .with_state(self.clone())
    }
}
fn envelope(value: Value, client: u32, server: u32) -> Value {
    json!({"Value":value,"ClientTransactionID":client,"ServerTransactionID":server,"ErrorNumber":0,"ErrorMessage":""})
}
fn error_code(e: &anyhow::Error) -> i32 {
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
    let value=match member.as_str(){"description"=>json!({"ServerName":"ZWOgain","Manufacturer":"Yann Ramin","ManufacturerVersion":env!("CARGO_PKG_VERSION"),"Location":"Camera server"}),
        "configureddevices"=>json!(s.profiles.all().into_iter().enumerate().filter(|(_,p)|p.camera.is_some()).map(|(slot,p)|json!({"DeviceName":p.label,"DeviceType":"Camera","DeviceNumber":slot,"UniqueID":p.unique_id})).collect::<Vec<_>>()),_=>return StatusCode::NOT_FOUND.into_response()};
    Json(envelope(value, id, s.next())).into_response()
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
    let Ok(device) = s.device(slot) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if member != member.to_lowercase() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let server = s.next();
    let mut client_transaction = 0;
    let image =
        method == Method::GET && matches!(member.as_str(), "imagearray" | "imagearrayvariant");
    let binary = headers
        .get("accept")
        .and_then(|s| s.to_str().ok())
        .is_some_and(|s| {
            s.split(',').any(|m| {
                let mut parts = m.trim().split(';');
                parts.next() == Some("application/imagebytes") && !parts.any(|v| v.trim() == "q=0")
            })
        });
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
fn setup_allowed(headers: &HeaderMap) -> bool {
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
    fn runtime() -> Runtime {
        Runtime {
            directory: std::env::var_os("ZWOGAIN_TEST_WORKERS")
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
            exposure: zwogain_core::Exposure {
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
            .chunks_exact(2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .collect();
        assert_eq!(values, [1, 4, 2, 50000, 65535, 6]);
        let response = image_response(frame, false, 7, 8);
        let data = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&data).unwrap();
        assert_eq!(value["Value"], json!([[1, 4], [2, 50000], [65535, 6]]));
    }
    #[tokio::test]
    async fn sdk_and_direct_camera_contract_over_http() {
        for direct in [false, true] {
            let runtime = runtime();
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
                ("numx", "NumX=64"),
                ("numy", "NumY=64"),
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
            assert_eq!(image["Value"].as_array().unwrap().len(), 64);
            assert_eq!(image["Value"][0].as_array().unwrap().len(), 64);
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
