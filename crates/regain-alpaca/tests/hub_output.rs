//! Real private endpoint and production HTTP router, using explicit simulation.
#[path = "support/hub_camera_output.rs"]
mod camera;
#[path = "support/hub_covercalibrator_output.rs"]
mod covercalibrator;
#[path = "support/hub_filterwheel_output.rs"]
mod filterwheel;
#[path = "support/hub_protocol.rs"]
mod protocol;
#[path = "support/hub_rotator_output.rs"]
mod rotator;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use regain_alpaca::{
    hub_output::Publisher,
    profile::Profiles,
    server::{Log, Server},
};
use regain_core::{CancellationToken, Runtime};
use regain_hub::{
    config::{HubConfig, VirtualDevice},
    credentials::CredentialStore,
    endpoint::Endpoint,
    host,
    ipc::Limits,
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    simulated::SimulationUpdate,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    config: HubConfig,
    hub: Arc<HubRuntime>,
    server: Arc<Server>,
    router: Router,
    credentials: Arc<CredentialStore>,
    stop: CancellationToken,
    host: tokio::task::JoinHandle<Result<(), host::HostError>>,
}

// A private upstream exercises the production Alpaca adapter, shared host and
// HTTP publisher without opening SDKs, COM drivers or physical serial ports.
type AccessoryWrite = (String, std::collections::BTreeMap<String, String>);
struct AccessoryUpstream {
    values: Arc<std::sync::Mutex<std::collections::BTreeMap<String, Value>>>,
    writes: Arc<std::sync::Mutex<Vec<AccessoryWrite>>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
    lose_move_reply: Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<()>,
    source: regain_hub::config::SourceConfig,
}
impl AccessoryUpstream {
    async fn focuser() -> Self {
        Self::start(regain_hub::config::DeviceType::Focuser, 3).await
    }
    async fn rotator(version: u16) -> Self {
        Self::start(regain_hub::config::DeviceType::Rotator, version).await
    }
    async fn filterwheel(version: u16) -> Self {
        Self::start(regain_hub::config::DeviceType::FilterWheel, version).await
    }
    async fn covercalibrator(version: u16) -> Self {
        Self::start(regain_hub::config::DeviceType::CoverCalibrator, version).await
    }
    async fn start(kind: regain_hub::config::DeviceType, version: u16) -> Self {
        use regain_hub::config::{ConnectionPolicy, DeviceType, SourceBackend, SourceConfig};
        use regain_hub::parameters::PollPolicy;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = SourceConfig {
            id: uuid::Uuid::new_v4(),
            label: format!(
                "Private loopback {}",
                regain_alpaca::hub_output::class_name(kind)
            ),
            polling: PollPolicy {
                request_timeout_seconds: 0.1,
                poll_seconds: 1.0,
                ..PollPolicy::default()
            },
            backend: SourceBackend::Alpaca {
                base_url: format!("http://{}/", listener.local_addr().unwrap()),
                device_type: kind,
                device_number: 19,
                connection_policy: ConnectionPolicy::Managed,
                credential_reference: None,
            },
        };
        let modern = version
            >= match kind {
                DeviceType::CoverCalibrator => 2,
                DeviceType::FilterWheel => 3,
                _ => 4,
            };
        let initial = if kind == DeviceType::CoverCalibrator {
            vec![
                ("brightness".into(), json!(0)),
                ("maxbrightness".into(), json!(4096)),
                ("coverstate".into(), json!(1)),
                ("calibratorstate".into(), json!(1)),
                ("covermoving".into(), json!(false)),
                ("calibratorchanging".into(), json!(false)),
            ]
        } else if kind == DeviceType::FilterWheel {
            vec![
                ("names".into(), json!(["L", "Hα", ""])),
                ("focusoffsets".into(), json!([-12, 0, 17])),
                ("position".into(), json!(0)),
            ]
        } else if kind == DeviceType::Focuser {
            vec![
                ("absolute".into(), json!(true)),
                ("maxstep".into(), json!(1000)),
                ("maxincrement".into(), json!(100)),
                ("tempcompavailable".into(), json!(true)),
                ("position".into(), json!(50)),
                ("ismoving".into(), json!(false)),
                ("tempcomp".into(), json!(true)),
                ("temperature".into(), json!(-5.0)),
            ]
        } else {
            vec![
                ("canreverse".into(), json!(true)),
                ("ismoving".into(), json!(false)),
                ("mechanicalposition".into(), json!(350.0)),
                ("position".into(), json!(20.0)),
                ("reverse".into(), json!(false)),
                ("stepsize".into(), json!(0.02)),
                ("targetposition".into(), json!(20.0)),
            ]
        };
        let values = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
            String,
            Value,
        >::from_iter(initial)));
        let writes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let lose_move_reply = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state = (
            values.clone(),
            writes.clone(),
            connected.clone(),
            lose_move_reply.clone(),
        );
        let router = Router::new().fallback(axum::routing::any(
            move |uri: axum::http::Uri, method: axum::http::Method, body: axum::body::Bytes| {
                let (values, writes, connected, lose_move_reply) = state.clone();
                async move {
                    use std::sync::atomic::Ordering::SeqCst;
                    assert!(uri.path().starts_with(&format!("/api/v1/{}/19/", regain_alpaca::hub_output::class_name(kind).to_lowercase())));
                    let member = uri.path().rsplit('/').next().unwrap();
                    let args: std::collections::BTreeMap<String, String> = serde_urlencoded::from_str(
                        if method == "PUT" { std::str::from_utf8(&body).unwrap() } else { uri.query().unwrap_or("") }
                    ).unwrap();
                    assert!(args["ClientID"].parse::<u32>().unwrap() > 0);
                    assert!(args["ClientTransactionID"].parse::<u32>().unwrap() > 0);
                    let value = if method == "PUT" {
                        writes.lock().unwrap().push((member.into(), args.clone()));
                        match member {
                            "connected" => { assert!(!modern); connected.store(args["Connected"] == "true", SeqCst); },
                            "connect" | "disconnect" => { assert!(modern); connected.store(member == "connect", SeqCst); },
                            "opencover" | "closecover" | "haltcover" | "calibratoron" | "calibratoroff" if kind == DeviceType::CoverCalibrator => {
                                assert_eq!(args.len(), if member == "calibratoron" { 3 } else { 2 });
                                {
                                    let mut state = values.lock().unwrap();
                                    match member {
                                        "opencover" | "closecover" => {
                                            state.insert("coverstate".into(), json!(2));
                                            state.insert("covermoving".into(), json!(true));
                                        },
                                        "haltcover" => {
                                            state.insert("coverstate".into(), json!(4));
                                            state.insert("covermoving".into(), json!(false));
                                        },
                                        "calibratoron" => {
                                            let brightness = args["Brightness"].parse::<i32>().unwrap();
                                            assert!((0..=state["maxbrightness"].as_i64().unwrap()).contains(&i64::from(brightness)));
                                            state.insert("brightness".into(), json!(brightness));
                                            state.insert("calibratorstate".into(), json!(2));
                                            state.insert("calibratorchanging".into(), json!(true));
                                        },
                                        "calibratoroff" => {
                                            state.insert("brightness".into(), json!(0));
                                            state.insert("calibratorstate".into(), json!(1));
                                            state.insert("calibratorchanging".into(), json!(false));
                                        },
                                        _ => unreachable!(),
                                    }
                                }
                                if lose_move_reply.load(SeqCst) { tokio::time::sleep(Duration::from_secs(1)).await; }
                            },
                            "position" if kind == DeviceType::FilterWheel => {
                                assert!(args["Position"].parse::<i32>().unwrap() >= 0);
                                values.lock().unwrap().insert("position".into(),json!(-1));
                                if lose_move_reply.load(SeqCst) {tokio::time::sleep(Duration::from_secs(1)).await;}
                            },
                            "move" | "moveabsolute" | "movemechanical" if kind == DeviceType::Rotator => {
                                let degrees = args["Position"].parse::<f64>().unwrap();
                                {
                                    let mut state = values.lock().unwrap();
                                    let logical = state["position"].as_f64().unwrap(); let mechanical = state["mechanicalposition"].as_f64().unwrap();
                                    let target = match member { "move" => logical + degrees, "movemechanical" => degrees + logical - mechanical, _ => degrees };
                                    state.insert("targetposition".into(), json!(target.rem_euclid(360.0)));
                                    state.insert("ismoving".into(), json!(true));
                                }
                                if lose_move_reply.load(SeqCst) { tokio::time::sleep(Duration::from_secs(1)).await; }
                            },
                            "sync" if kind == DeviceType::Rotator => {
                                let degrees = args["Position"].parse::<f64>().unwrap(); let mut state = values.lock().unwrap();
                                state.insert("position".into(), json!(degrees)); state.insert("targetposition".into(), json!(degrees));
                            },
                            "reverse" if kind == DeviceType::Rotator => { values.lock().unwrap().insert("reverse".into(), json!(args["Reverse"] == "true")); },
                            "move" => {
                                values.lock().unwrap().insert("position".into(), json!(args["Position"].parse::<i32>().unwrap()));
                                values.lock().unwrap().insert("ismoving".into(), json!(true));
                                if lose_move_reply.load(SeqCst) { tokio::time::sleep(Duration::from_secs(1)).await; }
                            }
                            "halt" => { values.lock().unwrap().insert("ismoving".into(), json!(false)); }
                            "tempcomp" => { values.lock().unwrap().insert("tempcomp".into(), json!(args["TempComp"] == "true")); }
                            _ => panic!("Unexpected focuser write: {member}"),
                        }
                        Value::Null
                    } else {
                        match member {
                            "interfaceversion" => json!(version),
                            "connected" => json!(connected.load(SeqCst)),
                            "connecting" => { assert!(modern); json!(false) },
                            "covermoving" | "calibratorchanging" if kind == DeviceType::CoverCalibrator && !modern => {
                                return axum::Json(json!({"ErrorNumber":1024,"ErrorMessage":"V1 has no completion Boolean"}));
                            },
                            _ => match values.lock().unwrap().get(member) {
                                Some(value) => value.clone(),
                                None => return axum::Json(json!({"ErrorNumber":1024,"ErrorMessage":"private detail must not escape"})),
                            },
                        }
                    };
                    axum::Json(json!({"ErrorNumber":0,"Value":value}))
                }
            }
        ));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            values,
            writes,
            connected,
            lose_move_reply,
            task,
            source,
        }
    }
    fn config(&self, numbers: &[u32]) -> HubConfig {
        let mut config = HubConfig::empty();
        config.sources.push(self.source.clone());
        for &number in numbers {
            config.outputs.push(regain_hub::config::OutputConfig {
                id: uuid::Uuid::new_v4(),
                number,
                label: format!("Private accessory {number}"),
                device: VirtualDevice::Proxy {
                    source: self.source.id,
                    device_type: match self.source.backend {
                        regain_hub::config::SourceBackend::Alpaca { device_type, .. } => {
                            device_type
                        }
                        _ => unreachable!(),
                    },
                },
            });
        }
        config
    }
    fn moves(&self) -> usize {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .filter(|(member, _)| member == "move")
            .count()
    }
    async fn finish(self) {
        self.task.abort();
        assert!(self.task.await.unwrap_err().is_cancelled());
    }
}

#[tokio::test]
async fn focuser_publication_preserves_dynamic_identity_and_shared_client_ownership() {
    let upstream = AccessoryUpstream::focuser().await;
    let other = AccessoryUpstream::focuser().await;
    let mut config = upstream.config(&[4, 7]);
    let second = other.config(&[12]);
    config.sources.extend(second.sources);
    config.outputs.extend(second.outputs);
    let f = Fixture::from_config(config).await;
    let devices = f
        .ok(
            "GET",
            "/management/v1/configureddevices",
            "ClientTransactionID=31",
        )
        .await;
    assert_eq!(devices.as_array().unwrap().len(), 3);
    for output in &f.config.outputs {
        let device = devices
            .as_array()
            .unwrap()
            .iter()
            .find(|device| device["UniqueID"] == json!(output.id))
            .unwrap();
        assert_eq!(device["DeviceNumber"], output.number);
        assert_eq!(device["DeviceType"], "Focuser");
        assert_eq!(device["DeviceName"], output.label);
    }
    assert!(upstream.writes.lock().unwrap().is_empty());
    assert!(other.writes.lock().unwrap().is_empty());
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/4/interfaceversion", "").await,
        4
    );
    assert_eq!(
        f.call("GET", "/api/v1/focuser/4/position", "ClientID=1")
            .await["ErrorNumber"],
        0x407
    );
    for slot in [0, 5, 19] {
        assert_eq!(
            request(
                &f.router,
                "GET",
                &format!("/api/v1/focuser/{slot}/connected"),
                ""
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    // Local slots retain their own routes when their numbers do not collide.
    assert_eq!(
        f.server
            .profiles
            .focusers
            .add(regain_alpaca::slots::Kind::Fc3)
            .unwrap(),
        0
    );
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/0/connected", "ClientID=90")
            .await,
        false
    );
    assert!(
        f.ok("GET", "/api/v1/focuser/0/name", "")
            .await
            .as_str()
            .unwrap()
            .contains("FocusCube")
    );
    assert_eq!(
        f.ok("GET", "/management/v1/configureddevices", "")
            .await
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let response = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/setup/v1/focuser/4/setup")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let page = response.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&page).unwrap().contains("hub.mjs"));
    f.ok(
        "PUT",
        "/api/v1/focuser/4/connected",
        "ClientID=1&Connected=true",
    )
    .await;
    f.ok("PUT", "/api/v1/focuser/7/connect", "ClientID=2").await;
    eventually(async || {
        f.ok("GET", "/api/v1/focuser/7/connecting", "ClientID=2")
            .await
            == false
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/7/connected", "ClientID=2")
            .await,
        true
    );
    for (member, value) in [
        ("absolute", json!(true)),
        ("maxstep", json!(1000)),
        ("maxincrement", json!(100)),
        ("tempcompavailable", json!(true)),
        ("tempcomp", json!(true)),
        ("ismoving", json!(false)),
        ("position", json!(50)),
        ("temperature", json!(-5.0)),
    ] {
        assert_eq!(
            f.ok("GET", &format!("/api/v1/focuser/4/{member}"), "ClientID=1")
                .await,
            value
        );
    }
    let optional = f
        .call("GET", "/api/v1/focuser/4/stepsize", "ClientID=1")
        .await;
    assert_eq!(optional["ErrorNumber"], 0x400);
    assert!(!optional.to_string().contains("private detail"));
    for value in ["-1", "1001", "200", "2147483648", "1.5", "bogus"] {
        let rejected = f
            .call(
                "PUT",
                "/api/v1/focuser/4/move",
                &format!("ClientID=1&Position={value}"),
            )
            .await;
        assert_eq!(rejected["ErrorNumber"], 0x401, "{rejected}");
    }
    assert_eq!(upstream.moves(), 0);
    let moved = f
        .call(
            "PUT",
            "/api/v1/focuser/4/move",
            "ClientID=1&ClientTransactionID=87&Position=123",
        )
        .await;
    assert_eq!(moved["ErrorNumber"], 0);
    assert_eq!(moved["ClientTransactionID"], 87);
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/7/ismoving", "ClientID=2")
            .await,
        true
    );
    assert_eq!(
        f.call("PUT", "/api/v1/focuser/7/move", "ClientID=2&Position=130")
            .await["ErrorNumber"],
        0x40b
    );
    f.ok("PUT", "/api/v1/focuser/7/halt", "ClientID=2").await;
    f.ok(
        "PUT",
        "/api/v1/focuser/7/tempcomp",
        "ClientID=2&TempComp=false",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/4/tempcomp", "ClientID=1")
            .await,
        false
    );
    eventually(async || {
        f.hub
            .source_snapshot(upstream.source.id)
            .unwrap()
            .values
            .get("position")
            == Some(&json!(123))
    })
    .await;
    let state = f
        .ok("GET", "/api/v1/focuser/4/devicestate", "ClientID=1")
        .await;
    assert!(
        state
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["Name"] == "Position" && entry["Value"] == 123)
    );
    assert!(
        state
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["Name"] != "TimeStamp" && entry["Name"] != "StepSize")
    );
    f.ok(
        "PUT",
        "/api/v1/focuser/12/connected",
        "ClientID=3&Connected=true",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/12/position", "ClientID=3")
            .await,
        50
    );
    f.ok(
        "PUT",
        "/api/v1/focuser/4/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    assert!(upstream.connected.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/7/position", "ClientID=2")
            .await,
        123
    );
    f.ok("PUT", "/api/v1/focuser/7/disconnect", "ClientID=2")
        .await;
    eventually(async || !upstream.connected.load(std::sync::atomic::Ordering::SeqCst)).await;
    let writes = upstream.writes.lock().unwrap().clone();
    assert_eq!(
        writes
            .iter()
            .map(|(member, _)| member.as_str())
            .collect::<Vec<_>>(),
        ["connected", "move", "halt", "tempcomp", "connected"]
    );
    assert!(
        writes
            .iter()
            .all(|(_, args)| args["ClientID"] == writes[0].1["ClientID"])
    );
    f.finish().await;
    upstream.finish().await;
    other.finish().await;
}

#[tokio::test]
async fn focuser_lost_move_reply_is_not_replayed_and_stale_clients_must_reconnect() {
    let upstream = AccessoryUpstream::focuser().await;
    let f = Fixture::from_config(upstream.config(&[4, 7])).await;
    for (slot, client) in [(4, 1), (7, 2)] {
        f.ok(
            "PUT",
            &format!("/api/v1/focuser/{slot}/connected"),
            &format!("ClientID={client}&Connected=true"),
        )
        .await;
    }
    upstream
        .lose_move_reply
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = f
        .call("PUT", "/api/v1/focuser/4/move", "ClientID=1&Position=123")
        .await;
    assert_eq!(result["ErrorNumber"], 0x500, "{result}");
    assert_eq!(upstream.moves(), 1);
    assert!(
        f.hub
            .source_snapshot(upstream.source.id)
            .unwrap()
            .write_uncertain
    );
    let result = f
        .call("PUT", "/api/v1/focuser/7/move", "ClientID=2&Position=124")
        .await;
    assert_eq!(result["ErrorNumber"], 0x500);
    assert!(
        result["ErrorMessage"]
            .as_str()
            .unwrap()
            .contains("do not replay")
    );
    assert_eq!(upstream.moves(), 1);
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/4/connected", "ClientID=1")
            .await,
        false
    );
    // Idempotent Connected=true never silently adopts the replacement generation.
    f.ok(
        "PUT",
        "/api/v1/focuser/4/connected",
        "ClientID=1&Connected=true",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/focuser/4/connected", "ClientID=1")
            .await,
        false
    );
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn focuser_publication_rejects_slot_collisions_before_any_equipment_connection() {
    let upstream = AccessoryUpstream::focuser().await;
    let f = Fixture::from_config(upstream.config(&[0])).await;
    assert_eq!(
        f.server
            .profiles
            .focusers
            .add(regain_alpaca::slots::Kind::Fc3)
            .unwrap(),
        0
    );
    assert_eq!(
        f.call("GET", "/management/v1/configureddevices", "").await["ErrorNumber"],
        0x401
    );
    for member in ["connected", "position"] {
        let reply = f
            .call(
                "GET",
                &format!("/api/v1/focuser/0/{member}"),
                "ClientID=1&ClientTransactionID=92",
            )
            .await;
        assert_eq!(reply["ErrorNumber"], 0x401);
        assert_eq!(reply["ClientTransactionID"], 92);
    }
    assert_eq!(
        f.router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/setup/v1/focuser/0/setup")
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(upstream.writes.lock().unwrap().is_empty());
    assert_eq!(f.hub.active_connections(), 0);
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn relative_focuser_rejects_position_and_preserves_signed_moves_and_strict_values() {
    let upstream = AccessoryUpstream::focuser().await;
    upstream
        .values
        .lock()
        .unwrap()
        .insert("absolute".into(), json!(false));
    let f = Fixture::from_config(upstream.config(&[4])).await;
    f.ok(
        "PUT",
        "/api/v1/focuser/4/connected",
        "ClientID=1&Connected=true",
    )
    .await;
    assert_eq!(
        f.call("GET", "/api/v1/focuser/4/position", "ClientID=1")
            .await["ErrorNumber"],
        0x400
    );
    for value in ["-101", "101", "-2147483648"] {
        assert_eq!(
            f.call(
                "PUT",
                "/api/v1/focuser/4/move",
                &format!("ClientID=1&Position={value}")
            )
            .await["ErrorNumber"],
            0x401
        );
    }
    f.ok("PUT", "/api/v1/focuser/4/move", "ClientID=1&Position=-30")
        .await;
    f.ok("PUT", "/api/v1/focuser/4/halt", "ClientID=1").await;
    assert_eq!(upstream.moves(), 1);
    assert_eq!(
        upstream
            .writes
            .lock()
            .unwrap()
            .iter()
            .find(|(member, _)| member == "move")
            .unwrap()
            .1["Position"],
        "-30"
    );
    for (member, value) in [
        ("ismoving", json!("false")),
        ("maxstep", json!(1.5)),
        ("temperature", json!("NaN")),
    ] {
        upstream.values.lock().unwrap().insert(member.into(), value);
        let reply = f
            .call("GET", &format!("/api/v1/focuser/4/{member}"), "ClientID=1")
            .await;
        assert_eq!(reply["ErrorNumber"], 0x402, "{reply}");
    }
    assert_eq!(
        f.call("PUT", "/api/v1/focuser/4/tempcomp", "ClientID=1&TempComp=1")
            .await["ErrorNumber"],
        0x401
    );
    assert_eq!(
        request(
            &f.router,
            "GET",
            "/api/v1/focuser/4/getswitch",
            "ClientID=1&Id=0"
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(
            &f.router,
            "PUT",
            "/api/v1/focuser/4/move",
            "ClientID=1&Position=1&position=2"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    f.finish().await;
    upstream.finish().await;
}

#[tokio::test]
async fn setup_output_diagnostics_are_same_origin_revision_checked_and_inert() {
    let f = Fixture::new().await;
    let command = json!({"op":"outputStatus","output":f.config.outputs[0].id,"expectedRevision":f.config.revision,"start":0,"limit":1});
    for (media, origin) in [
        ("application/json", "https://other.invalid"),
        ("text/plain", "http://127.0.0.1:11111"),
    ] {
        assert_eq!(
            setup(&f.router, command.clone(), media, origin).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let (code, observed) = setup(
        &f.router,
        command.clone(),
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{observed}");
    assert_eq!(observed["result"]["purpose"], "cachedDiagnostics");
    assert_eq!(observed["result"]["output"], json!(f.config.outputs[0].id));
    assert_eq!(
        observed["result"]["configurationRevision"],
        json!(f.config.revision)
    );
    assert_eq!(observed["result"]["simulated"], true);
    let mut stale = command.clone();
    stale["expectedRevision"] = json!(uuid::Uuid::new_v4());
    let (_, rejected) = setup(
        &f.router,
        stale,
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(rejected["error"]["code"], "revisionConflict");
    let mut missing = command;
    missing.as_object_mut().unwrap().remove("expectedRevision");
    assert_eq!(
        setup(
            &f.router,
            missing,
            "application/json",
            "http://127.0.0.1:11111"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(f.hub.active_connections(), 0);
    assert!(
        f.hub
            .source_snapshots()
            .iter()
            .all(|source| source.lease_count == 0
                && source.sequence == 0
                && !source.transport_connected)
    );
    f.finish().await;
}

#[tokio::test]
async fn setup_simulation_updates_are_sparse_revision_checked_and_same_origin() {
    let f = Fixture::new().await;
    let source = f.config.sources[0].id;
    let revision = f.config.revision;
    let command = json!({"op":"updateSimulation","source":source,"expectedRevision":revision,"update":{"switchValues":{"1":42.0}}});
    assert_eq!(
        setup(
            &f.router,
            command.clone(),
            "application/json",
            "http://other.invalid"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (code, _) = setup(&f.router,json!({"op":"updateSimulation","source":source,"expectedRevision":uuid::Uuid::new_v4(),"update":{"switchValues":{"1":42.0}}}),"application/json","http://127.0.0.1:11111").await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_eq!(
        f.hub
            .source_snapshot(source)
            .unwrap()
            .simulation
            .unwrap()
            .switch_values[&1],
        0.0
    );
    assert_eq!(f.hub.source_snapshot(source).unwrap().lease_count, 0);
    let (_, outcome) = setup(
        &f.router,
        command,
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(outcome["result"]["source"], json!(source), "{outcome}");
    assert_eq!(outcome["result"]["configurationRevision"], json!(revision));
    assert_eq!(outcome["result"]["simulation"]["switchValues"]["1"], 42.0);
    assert_eq!(outcome["result"]["simulation"]["switchValues"]["2"], 12.0);
    assert_eq!(outcome["result"]["simulation"]["fault"], "none");
    let status = f.hub.source_snapshot(source).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while f.hub.source_snapshot(source).unwrap().lease_count != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!status.write_uncertain);
    let (_, saved) = setup(
        &f.router,
        json!({"op":"getConfig"}),
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    let mut candidate = saved["result"].clone();
    candidate["sources"][0]["label"] = json!("Replacement simulated source");
    let (_, applied) = setup(
        &f.router,
        json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate}),
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(applied["result"]["ready"], true);
    let (_, rejected) = setup(&f.router,json!({"op":"updateSimulation","source":source,"expectedRevision":revision,"update":{"switchValues":{"1":99.0}}}),"application/json","http://127.0.0.1:11111").await;
    assert_eq!(rejected["error"]["code"], "revisionConflict");
    let (_, fresh) = setup(
        &f.router,
        json!({"op":"sourceStatus","source":source}),
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(fresh["result"]["leaseCount"], 0);
    assert_eq!(fresh["result"]["simulation"]["switchValues"]["1"], 0.0);
    let response = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/hub-simulation.mjs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .contains("SimulationSetup")
    );
    f.finish().await;
}
impl Fixture {
    async fn new() -> Self {
        let mut config: HubConfig = serde_json::from_str(include_str!(
            "../../regain-hub/examples/simulated-observatory.json"
        ))
        .unwrap();
        // Non-contiguous output numbers must survive discovery and routing.
        config.outputs[0].number = 7;
        config.outputs[1].number = 3;
        config.outputs[2].number = 12;
        for source in &mut config.sources {
            source.polling.poll_seconds = 0.1;
        }
        if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
            members[0].policy.safe_readings_to_safe = 1;
            members[0].policy.return_to_safe_hold_seconds = 0.0;
        }
        Self::from_config(config).await
    }
    async fn from_config(config: HubConfig) -> Self {
        Self::from_config_with_workers(config, None).await
    }
    async fn from_config_with_workers(
        config: HubConfig,
        workers: Option<std::path::PathBuf>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hub.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let endpoint = Endpoint::for_config(&path).unwrap();
        let listener = endpoint.try_lock().unwrap().unwrap().bind().unwrap();
        let native = if let Some(directory) = workers {
            assert!(
                directory
                    .join(format!("regain-device{}", std::env::consts::EXE_SUFFIX))
                    .is_file()
            );
            NativeRuntime {
                cameras: Some(regain_hub::camera::runtime::NativeCameraRuntime {
                    sdk: "unused-explicit-simulation".into(),
                    sdk_simulation: Some(json!({"instant":false})),
                    resources: Default::default(),
                    diagnostic: Arc::new(|_, _, _| {}),
                }),
                directory,
                simulate: true,
                references: Some(
                    regain_hub::native_reference::NativeReferenceStore::at_directory(
                        &dir.path().join("references"),
                        endpoint.key(),
                    )
                    .unwrap(),
                ),
            }
        } else {
            NativeRuntime {
                cameras: None,
                directory: dir.path().into(),
                simulate: false,
                references: None,
            }
        };
        let credentials =
            Arc::new(CredentialStore::at_directory(dir.path(), endpoint.key()).unwrap());
        let provider = credentials.clone();
        let service = regain_hub::service::HubService::persistent_with_credentials(
            regain_hub::config::ConfigStore::load(&path).unwrap(),
            Arc::new(move |config| {
                HubRuntime::build(
                    config,
                    &native,
                    &*provider,
                    Arc::new(MonotonicClock::default()),
                )
            }),
            credentials.clone(),
        )
        .unwrap();
        let hub = service.runtime().unwrap();
        let stop = CancellationToken::new();
        let host = tokio::spawn(host::serve_service(
            listener,
            service,
            Limits::default(),
            stop.clone(),
        ));
        let publisher = Publisher::connect(endpoint, config.instance_id)
            .await
            .unwrap();
        let profiles = dir.path().join("profiles.json");
        std::fs::write(&profiles, b"[]").unwrap();
        let server = Server::with_hub(
            Arc::new(Profiles::new(Some(profiles)).unwrap()),
            Runtime {
                directory: dir.path().into(),
                sdk: dir.path().join("unused"),
                simulate: false,
                sdk_simulation: None,
            },
            Log::new(None),
            Some(publisher),
        );
        let router = server.router();
        Self {
            _dir: dir,
            config,
            hub,
            server,
            router,
            credentials,
            stop,
            host,
        }
    }
    async fn finish(self) {
        self.server.shutdown().await;
        self.stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.host)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    async fn call(&self, method: &str, path: &str, data: &str) -> Value {
        let (status, body) = request(&self.router, method, path, data).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        body
    }
    async fn ok(&self, method: &str, path: &str, data: &str) -> Value {
        let body = self.call(method, path, data).await;
        assert_eq!(body["ErrorNumber"], 0, "{path}: {body}");
        body["Value"].clone()
    }
}

#[tokio::test]
async fn accessory_only_setup_has_no_dummy_cameras_and_can_add_its_first_slot() {
    let f = Fixture::new().await;
    assert_eq!(
        request(&f.router, "GET", "/setup/api/state", "").await.1["cameras"],
        json!([])
    );
    let devices = f.ok("GET", "/management/v1/configureddevices", "").await;
    assert_eq!(devices.as_array().unwrap().len(), 3);
    assert!(
        devices
            .as_array()
            .unwrap()
            .iter()
            .all(|device| device["DeviceType"] != "Camera")
    );
    for method in ["GET", "PUT"] {
        let reply = request(
            &f.router,
            method,
            "/api/v1/camera/0/connected",
            "ClientID=90&Connected=true",
        )
        .await;
        assert_eq!(reply.0, StatusCode::NOT_FOUND);
    }
    let response = f
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/setup/api/slots")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let added: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(added["slot"], 0);
    let state = request(&f.router, "GET", "/setup/api/state", "").await.1;
    assert_eq!(state["cameras"].as_array().unwrap().len(), 1);
    assert_eq!(state["cameras"][0]["slot"], 0);
    assert_eq!(state["cameras"][0]["connected"], false);
    let reopened = Profiles::new(Some(f._dir.path().join("profiles.json"))).unwrap();
    assert_eq!(
        json!(reopened.get(0).unwrap().unique_id),
        state["cameras"][0]["profile"]["uniqueId"]
    );
    assert_eq!(
        f.ok("GET", "/management/v1/configureddevices", "").await,
        devices
    );
    assert!(
        f.hub
            .source_snapshots()
            .iter()
            .all(|source| source.lease_count == 0 && !source.transport_connected)
    );
    f.finish().await;
}
async fn request(router: &Router, method: &str, path: &str, data: &str) -> (StatusCode, Value) {
    let uri = if method == "GET" && !data.is_empty() {
        format!("{path}?{data}")
    } else {
        path.into()
    };
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(Body::from(
            if method == "GET" { "" } else { data }.to_owned(),
        ))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn eventually(mut read: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !read().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

async fn setup(
    router: &Router,
    command: Value,
    content_type: &str,
    origin: &str,
) -> (StatusCode, Value) {
    setup_at(router, "/setup/api/hub", command, content_type, origin).await
}
async fn setup_at(
    router: &Router,
    path: &str,
    command: Value,
    content_type: &str,
    origin: &str,
) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("Content-Type", content_type)
                .header("Host", "127.0.0.1:11111")
                .header("Origin", origin)
                .body(Body::from(serde_json::to_vec(&command).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    if status != StatusCode::FORBIDDEN {
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn setup_routes_share_metadata_validate_and_apply_with_revision_and_connection_guards() {
    let f = Fixture::new().await;
    let invoke = async |command| {
        setup(
            &f.router,
            command,
            "application/json",
            "http://127.0.0.1:11111",
        )
        .await
    };
    let (status, metadata) = invoke(json!({"op":"describeConfig"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(metadata["result"]["contractVersion"], 1);
    assert_eq!(
        metadata["result"]["schema"]["$defs"]["SafetyMember"]["properties"]["source"]["x-regain"]["reference"],
        "source"
    );
    let (_, original) = invoke(json!({"op":"getConfig"})).await;
    let mut candidate = original["result"].clone();
    let revision = candidate["revision"].clone();
    candidate["outputs"][0]["label"] = json!("Updated hub switches");
    let (_, validated) = invoke(json!({"op":"validateConfig","candidate":candidate})).await;
    assert_eq!(validated["result"]["valid"], true);
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=10&Connected=true",
    )
    .await;
    let (status, _) =
        invoke(json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=10&Connected=false",
    )
    .await;
    eventually(async || f.hub.active_connections() == 0).await;
    let (status, applied) =
        invoke(json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate})).await;
    assert_eq!(status, StatusCode::OK, "{applied}");
    assert_eq!(applied["result"]["ready"], true);
    assert_ne!(applied["result"]["configurationRevision"], revision);
    assert!(
        f.ok("GET", "/api/v1/switch/7/name", "")
            .await
            .as_str()
            .unwrap()
            .contains("Updated hub switches")
    );
    assert_eq!(
        invoke(json!({"op":"applyConfig","expectedRevision":revision,"candidate":candidate}))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let persisted: HubConfig =
        serde_json::from_slice(&std::fs::read(f._dir.path().join("hub.json")).unwrap()).unwrap();
    assert_eq!(persisted.outputs[0].label, "Updated hub switches");
    assert_eq!(persisted.outputs[0].id, f.config.outputs[0].id);
    candidate["outputs"][0]["label"] = json!("");
    let (_, invalid) = invoke(json!({"op":"validateConfig","candidate":candidate})).await;
    assert_eq!(invalid["result"]["valid"], false);
    assert!(!invalid["result"]["errors"].as_array().unwrap().is_empty());
    f.finish().await;
}

#[tokio::test]
async fn web_credentials_share_private_storage_and_reject_cross_site_mutations() {
    let f = Fixture::new().await;
    let id = uuid::Uuid::new_v4();
    let reference = format!("credential-{id}");
    let secret = "Bearer private-web-fixture";
    let create = json!({"op":"createCredential","referenceId":id,"authorization":secret});
    for (media, origin) in [
        ("application/json", "http://untrusted.example"),
        ("text/plain", "http://127.0.0.1:11111"),
    ] {
        assert_eq!(
            setup(&f.router, create.clone(), media, origin).await.0,
            StatusCode::FORBIDDEN
        );
        assert!(!f.credentials.status(&reference).unwrap().present);
        assert_eq!(
            setup_at(&f.router, "/setup/api/hub/reload", json!({}), media, origin)
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }
    for path in ["/setup/api/hub", "/setup/api/hub/reload"] {
        let response = f
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(path)
                    .header("Content-Type", "application/json")
                    .header("Host", "127.0.0.1:11111")
                    .header("Origin", "http://127.0.0.1:11111")
                    .header("Sec-Fetch-Site", "cross-site")
                    .body(Body::from(serde_json::to_vec(&create).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!f.credentials.status(&reference).unwrap().present);
    }
    let invoke = async |command| {
        setup(
            &f.router,
            command,
            "application/json",
            "http://127.0.0.1:11111",
        )
        .await
    };
    let (status, description) = invoke(json!({"op":"describeConfig"})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        description["result"]["credentialStorage"]["clientChosenReferences"],
        true
    );
    let (status, result) = invoke(create).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["result"]["reference"], reference);
    assert!(!result.to_string().contains(secret));
    assert_eq!(
        invoke(json!({"op":"credentialStatus","reference":reference}))
            .await
            .1["result"]["present"],
        true
    );
    let (status, reload) = setup_at(
        &f.router,
        "/setup/api/hub/reload",
        json!({}),
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reload["result"]["instanceId"], json!(f.config.instance_id));
    assert_eq!(
        invoke(json!({"op":"credentialStatus","reference":reference}))
            .await
            .1["result"]["present"],
        true
    );
    assert_eq!(
        setup_at(
            &f.router,
            "/setup/api/hub/reload",
            json!({"authorization":secret}),
            "application/json",
            "http://127.0.0.1:11111"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for value in [json!([]), Value::Null, json!("")] {
        assert_eq!(
            setup_at(
                &f.router,
                "/setup/api/hub/reload",
                value,
                "application/json",
                "http://127.0.0.1:11111"
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let (_, original) = invoke(json!({"op":"getConfig"})).await;
    let mut candidate = original["result"].clone();
    candidate["sources"].as_array_mut().unwrap().push(json!({"id":uuid::Uuid::new_v4(),"label":"Unused authenticated source",
        "backend":{"kind":"alpaca","baseUrl":"http://127.0.0.1:1","deviceType":"safetymonitor","deviceNumber":0,"credentialReference":reference}}));
    assert_eq!(invoke(json!({"op":"applyConfig","expectedRevision":candidate["revision"],"candidate":candidate})).await.1["result"]["ready"],true);
    let (_, deletion) = invoke(json!({"op":"deleteCredential","reference":reference})).await;
    assert_eq!(deletion["error"]["code"], "inUse");
    assert!(!deletion.to_string().contains(secret));
    let (_, current) = invoke(json!({"op":"getConfig"})).await;
    assert!(!current.to_string().contains(secret));
    let mut candidate = current["result"].clone();
    candidate["sources"].as_array_mut().unwrap().pop();
    assert_eq!(invoke(json!({"op":"applyConfig","expectedRevision":candidate["revision"],"candidate":candidate})).await.1["result"]["ready"],true);
    assert_eq!(
        invoke(json!({"op":"deleteCredential","reference":reference}))
            .await
            .1["result"]["removed"],
        true
    );
    assert_eq!(
        invoke(json!({"op":"credentialStatus","reference":reference}))
            .await
            .1["result"]["present"],
        false
    );
    assert_eq!(f.hub.active_connections(), 0);
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=84&Connected=true",
    )
    .await;
    assert_eq!(
        setup_at(
            &f.router,
            "/setup/api/hub/reload",
            json!({}),
            "application/json",
            "http://127.0.0.1:11111"
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=84")
            .await,
        true
    );
    f.finish().await;
}

#[tokio::test]
async fn setup_rejects_cross_origin_wrong_media_type_and_device_commands_without_side_effects() {
    let f = Fixture::new().await;
    for (media, origin) in [
        ("application/json", "http://untrusted.example"),
        ("text/plain", "http://127.0.0.1:11111"),
        ("application/jsonp", "http://127.0.0.1:11111"),
    ] {
        assert_eq!(
            setup(
                &f.router,
                json!({"op":"inspectSource","source":f.config.sources[0].id,"start":0,"limit":8}),
                media,
                origin
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        setup(
            &f.router,
            json!({"op":"connect","output":f.config.outputs[0].id}),
            "application/json",
            "http://127.0.0.1:11111"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(f.hub.active_connections(), 0);
    assert_eq!(
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count,
        0
    );
    let (status, inspected) = setup(
        &f.router,
        json!({"op":"inspectSource","source":f.config.sources[0].id,"start":0,"limit":8}),
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    eventually(async || f.hub.active_connections() == 0).await;
    for path in [
        "/setup/hub",
        "/hub.mjs",
        "/hub-discovery.mjs",
        "/hub-config.mjs",
        "/hub-form.mjs",
        "/hub-credentials.mjs",
        "/hub.css",
        "/setup/v1/switch/7/setup",
        "/setup/v1/safetymonitor/3/setup",
        "/setup/v1/observingconditions/12/setup",
    ] {
        assert_eq!(request(&f.router, "GET", path, "").await.0, StatusCode::OK);
    }
    for path in [
        "/setup/v1/switch/0/setup",
        "/setup/v1/safetymonitor/7/setup",
        "/setup/v1/unsupported/7/setup",
    ] {
        assert_eq!(
            request(&f.router, "GET", path, "").await.0,
            StatusCode::NOT_FOUND
        );
    }
    f.finish().await;
}

#[tokio::test]
async fn setup_catalog_query_uses_private_ipc_and_management_without_source_leases() {
    let f = Fixture::new().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = f.router.clone();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let command =
        json!({"op":"discoverAlpaca", "baseUrl":url, "expectedRevision":f.config.revision});
    assert_eq!(
        setup(
            &f.router,
            command.clone(),
            "application/json",
            "http://untrusted.example"
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, response) = setup(
        &f.router,
        command,
        "application/json",
        "http://127.0.0.1:11111",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let catalog = &response["result"];
    assert_eq!(catalog["configurationRevision"], json!(f.config.revision));
    assert_eq!(catalog["baseUrl"], url);
    let entries = catalog["devices"].as_array().unwrap();
    assert_eq!(entries.len(), f.config.outputs.len());
    for (entry, output) in entries.iter().zip(&f.config.outputs) {
        assert_eq!(entry["uniqueId"], json!(output.id));
        assert_eq!(entry["number"], output.number);
        assert!(entry["supportedDeviceType"].is_string());
    }
    assert_eq!(f.hub.active_connections(), 0);
    for source in &f.config.sources {
        let snapshot = f.hub.source_snapshot(source.id).unwrap();
        assert_eq!(snapshot.lease_count, 0);
        assert!(!snapshot.transport_connected);
    }
    task.abort();
    let _ = task.await;
    f.finish().await;
}

#[tokio::test]
async fn dynamic_discovery_scalar_mapping_and_independent_client_leases() {
    let f = Fixture::new().await;
    let devices = f.ok("GET", "/management/v1/configureddevices", "").await;
    assert_eq!(devices.as_array().unwrap().len(), 3);
    for (actual, expected) in devices.as_array().unwrap().iter().zip(&f.config.outputs) {
        assert_eq!(actual["UniqueID"], json!(expected.id));
        assert_eq!(actual["DeviceNumber"], expected.number);
        assert!(
            actual["DeviceName"]
                .as_str()
                .unwrap()
                .contains("Simulation")
        );
    }
    assert_eq!(
        request(&f.router, "GET", "/api/v1/switch/0/name", "")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/interfaceversion", "").await,
        3
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=10")
            .await,
        false
    );
    assert_eq!(
        f.call("GET", "/api/v1/switch/7/getswitchvalue", "ClientID=10&Id=0")
            .await["ErrorNumber"],
        0x407
    );
    for id in [10, 20] {
        f.ok(
            "PUT",
            "/api/v1/switch/7/connected",
            &format!("ClientID={id}&Connected=true"),
        )
        .await;
    }
    assert_eq!(
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count,
        2
    );
    eventually(async || {
        f.call("GET", "/api/v1/switch/7/getswitchvalue", "ClientID=10&Id=0")
            .await["ErrorNumber"]
            == 0
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/maxswitch", "ClientID=10")
            .await,
        3
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/canwrite", "ClientID=10&Id=2")
            .await,
        false
    );
    let response = f
        .call(
            "PUT",
            "/api/v1/switch/7/setswitchvalue",
            "ClientID=10&ClientTransactionID=4294967295&Id=1&Value=37",
        )
        .await;
    assert_eq!(response["ErrorNumber"], 0);
    assert_eq!(response["ClientTransactionID"], u32::MAX);
    eventually(async || {
        f.call("GET", "/api/v1/switch/7/getswitchvalue", "ClientID=20&Id=1")
            .await["Value"]
            == 37.0
    })
    .await;
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=10&Connected=false",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=10")
            .await,
        false
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=20")
            .await,
        true
    );
    eventually(async || {
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count
            == 1
    })
    .await;
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=20&Connected=false",
    )
    .await;
    eventually(async || {
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count
            == 0
    })
    .await;
    f.finish().await;
}

#[tokio::test]
async fn device_state_preserves_slots_and_omits_failed_stale_or_unconfigured_values() {
    use regain_hub::config::{ConfigStore, WeatherMetric};
    let config: HubConfig = serde_json::from_str(include_str!(
        "../../regain-hub/examples/simulated-observatory.json"
    ))
    .unwrap();
    let store = ConfigStore::new(None, config).unwrap();
    let original = store.snapshot();
    let mut edited = original.clone();
    if let VirtualDevice::Switch { channels } = &mut edited.outputs[0].device {
        channels.remove(0);
    }
    if let VirtualDevice::Weather { measurements } = &mut edited.outputs[2].device {
        let mut metric = measurements[&WeatherMetric::Temperature].clone();
        if let regain_hub::config::Readout::Property { property, .. } = &mut metric.sources[0] {
            *property = "starfwhm".into();
        }
        measurements.insert(WeatherMetric::StarFwhm, metric);
    }
    store.apply(original.revision, edited, false).unwrap();
    let f = Fixture::from_config(store.snapshot()).await;
    for kind in ["switch", "safetymonitor", "observingconditions"] {
        assert_eq!(
            f.call(
                "GET",
                &format!("/api/v1/{kind}/0/devicestate"),
                "ClientID=7"
            )
            .await["ErrorNumber"],
            0x407
        );
        f.ok(
            "PUT",
            &format!("/api/v1/{kind}/0/connected"),
            "ClientID=7&Connected=true",
        )
        .await;
    }
    eventually(async || {
        f.ok("GET", "/api/v1/switch/0/devicestate", "ClientID=7")
            .await
            .as_array()
            .unwrap()
            .len()
            == 4
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/0/maxswitch", "ClientID=7")
            .await,
        3
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/0/devicestate", "ClientID=7")
            .await,
        json!([
            {"Name":"GetSwitch1","Value":false}, {"Name":"GetSwitchValue1","Value":0.0},
            {"Name":"GetSwitch2","Value":true}, {"Name":"GetSwitchValue2","Value":12.0}
        ])
    );
    eventually(async || {
        f.ok(
            "GET",
            "/api/v1/observingconditions/0/devicestate",
            "ClientID=7",
        )
        .await
        .as_array()
        .unwrap()
        .len()
            == 3
    })
    .await;
    let weather = f
        .ok(
            "GET",
            "/api/v1/observingconditions/0/devicestate",
            "ClientID=7",
        )
        .await;
    assert!(
        weather
            .as_array()
            .unwrap()
            .contains(&json!({"Name":"StarFWHM","Value":2.0}))
    );
    f.hub
        .update_simulation(
            f.config.sources[2].id,
            SimulationUpdate {
                weather: std::collections::BTreeMap::from([(WeatherMetric::Pressure, None)]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        let weather = f
            .ok(
                "GET",
                "/api/v1/observingconditions/0/devicestate",
                "ClientID=7",
            )
            .await;
        weather.as_array().unwrap().len() == 2
            && weather
                .as_array()
                .unwrap()
                .iter()
                .all(|state| state["Name"] != "Pressure")
    })
    .await;
    f.hub
        .update_simulation(
            f.config.sources[2].id,
            SimulationUpdate {
                sample_age_seconds: Some(120.0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        f.ok(
            "GET",
            "/api/v1/observingconditions/0/devicestate",
            "ClientID=7",
        )
        .await
            == json!([])
    })
    .await;
    f.hub
        .update_simulation(
            f.config.sources[0].id,
            SimulationUpdate {
                sample_age_seconds: Some(120.0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        f.ok("GET", "/api/v1/switch/0/devicestate", "ClientID=7")
            .await
            == json!([])
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/0/devicestate", "ClientID=7")
            .await,
        json!([
            {"Name":"IsSafe","Value":false}
        ])
    );
    f.finish().await;
}

#[tokio::test]
async fn weather_and_safety_use_host_evidence_and_never_http_cached_permission() {
    let f = Fixture::new().await;
    for path in [
        "/api/v1/safetymonitor/3/connected",
        "/api/v1/observingconditions/12/connected",
    ] {
        f.ok("PUT", path, "ClientID=1&Connected=true").await;
    }
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/3/issafe", "ClientID=1")
            .await,
        false
    );
    f.hub
        .update_simulation(
            f.config.sources[1].id,
            SimulationUpdate {
                safe: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        f.ok("GET", "/api/v1/safetymonitor/3/issafe", "ClientID=1")
            .await
            == true
    })
    .await;
    eventually(async || {
        f.call(
            "GET",
            "/api/v1/observingconditions/12/temperature",
            "ClientID=1",
        )
        .await["ErrorNumber"]
            == 0
    })
    .await;
    assert!(
        f.ok(
            "GET",
            "/api/v1/observingconditions/12/temperature",
            "ClientID=1"
        )
        .await
        .is_number()
    );
    assert!(
        f.ok(
            "GET",
            "/api/v1/observingconditions/12/sensordescription",
            "ClientID=1&SensorName=Temperature"
        )
        .await
        .as_str()
        .unwrap()
        .contains("temperature")
    );
    assert!(
        f.ok(
            "GET",
            "/api/v1/observingconditions/12/timesincelastupdate",
            "ClientID=1&SensorName=temperature"
        )
        .await
        .as_f64()
        .unwrap()
            >= 0.0
    );
    assert_eq!(
        f.call(
            "GET",
            "/api/v1/observingconditions/12/humidity",
            "ClientID=1"
        )
        .await["ErrorNumber"],
        0x400
    );
    f.ok(
        "PUT",
        "/api/v1/observingconditions/12/averageperiod",
        "ClientID=1&AveragePeriod=0.01",
    )
    .await;
    assert_eq!(
        f.ok(
            "GET",
            "/api/v1/observingconditions/12/averageperiod",
            "ClientID=1"
        )
        .await,
        0.01
    );
    f.ok(
        "PUT",
        "/api/v1/observingconditions/12/refresh",
        "ClientID=1",
    )
    .await;
    f.hub
        .update_simulation(
            f.config.sources[1].id,
            SimulationUpdate {
                safe: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    eventually(async || {
        f.ok("GET", "/api/v1/safetymonitor/3/issafe", "ClientID=1")
            .await
            == false
    })
    .await;
    f.stop.cancel();
    eventually(async || {
        f.call("GET", "/api/v1/safetymonitor/3/issafe", "ClientID=1")
            .await["ErrorNumber"]
            != 0
    })
    .await;
    f.finish().await;
}

#[tokio::test]
async fn invalid_requests_are_rejected_without_mutation_and_shutdown_releases_only_http_clients() {
    let f = Fixture::new().await;
    let native = f.hub.client();
    native.connect(f.config.outputs[0].id).await.unwrap();
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=42&Connected=true",
    )
    .await;
    for data in [
        "ClientID=42&Id=-1&Value=1",
        "ClientID=42&Id=1&Value=NaN",
        "ClientID=42&Id=1&Value=101",
    ] {
        assert_eq!(
            f.call("PUT", "/api/v1/switch/7/setswitchvalue", data).await["ErrorNumber"],
            0x401
        );
    }
    assert_eq!(
        request(
            &f.router,
            "PUT",
            "/api/v1/switch/7/setswitchvalue",
            "ClientID=42&Id=1&Value=1&id=2"
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/switch/7/setswitchname",
            "ClientID=42&Id=1&Name=changed"
        )
        .await["ErrorNumber"],
        0x400
    );
    assert_eq!(
        request(&f.router, "GET", "/api/v1/switch/7/issafe", "ClientID=42")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    f.server.shutdown().await;
    eventually(async || {
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count
            == 1
    })
    .await;
    assert!(native.connection(f.config.outputs[0].id).is_ok());
    native.close();
    f.finish().await;
}

#[tokio::test]
async fn bounded_http_clients_reuse_capacity_and_uncertain_writes_are_not_replayed() {
    let f = Fixture::new().await;
    for id in 1..=24 {
        f.ok(
            "PUT",
            "/api/v1/switch/7/connected",
            &format!("ClientID={id}&Connected=true"),
        )
        .await;
    }
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/switch/7/connected",
            "ClientID=25&Connected=true"
        )
        .await["ErrorNumber"],
        0x40b
    );
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=1&Connected=false",
    )
    .await;
    f.ok(
        "PUT",
        "/api/v1/switch/7/connected",
        "ClientID=25&Connected=true",
    )
    .await;
    f.hub
        .update_simulation(
            f.config.sources[0].id,
            SimulationUpdate {
                fault: Some(regain_hub::simulated::Fault::UncertainWrite),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let result = f
        .call(
            "PUT",
            "/api/v1/switch/7/setswitchvalue",
            "ClientID=25&Id=1&Value=33",
        )
        .await;
    assert_eq!(result["ErrorNumber"], 0x500);
    assert!(
        result["ErrorMessage"]
            .as_str()
            .unwrap()
            .contains("uncertain")
    );
    assert!(
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .write_uncertain
    );
    f.hub
        .update_simulation(
            f.config.sources[0].id,
            SimulationUpdate {
                fault: Some(regain_hub::simulated::Fault::None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/switch/7/setswitchvalue",
            "ClientID=2&Id=1&Value=44"
        )
        .await["ErrorNumber"],
        0x500
    );
    eventually(async || {
        f.call("GET", "/api/v1/switch/7/getswitchvalue", "ClientID=2&Id=1")
            .await["Value"]
            == 33.0
    })
    .await;
    f.finish().await;
}

#[tokio::test]
async fn slow_upstream_connection_does_not_block_other_outputs_or_claim_valid_readings() {
    use axum::{Json, extract::Request as AxumRequest, routing::any};
    use regain_hub::config::{ConnectionPolicy, DeviceType, SourceBackend};
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let upstream = Router::new().fallback(any({
        let entered = entered.clone(); let release = release.clone();
        move |request: AxumRequest| {
            let entered = entered.clone(); let release = release.clone();
            async move {
                let path = request.uri().path().to_owned();
                let query = request.uri().query().unwrap_or("").to_owned();
                let put = request.method() == "PUT";
                let bytes = request.into_body().collect().await.unwrap().to_bytes();
                let params = regain_alpaca::device::Params::parse(if put {
                    std::str::from_utf8(&bytes).unwrap()
                } else { &query }).unwrap();
                let transaction = params.optional_id("ClientTransactionID").unwrap();
                let value = if path.ends_with("/interfaceversion") { json!(2) }
                    else if put && path.ends_with("/connected") {
                        if String::from_utf8_lossy(&bytes).contains("Connected=true") {
                            entered.notify_one(); release.notified().await;
                        }
                        Value::Null
                    } else if path.ends_with("/connected") { json!(false) }
                    else { json!(0) };
                Json(json!({"Value":value,"ErrorNumber":0,"ErrorMessage":"","ClientTransactionID":transaction,"ServerTransactionID":1}))
            }
        }
    }));
    let serving = tokio::spawn(async move {
        axum::serve(listener, upstream).await.unwrap();
    });
    let mut config: HubConfig = serde_json::from_str(include_str!(
        "../../regain-hub/examples/simulated-observatory.json"
    ))
    .unwrap();
    config.sources[0].backend = SourceBackend::Alpaca {
        base_url: format!("http://{address}"),
        device_type: DeviceType::Switch,
        device_number: 0,
        connection_policy: ConnectionPolicy::Managed,
        credential_reference: None,
    };
    let f = Fixture::from_config(config).await;
    let connecting = tokio::spawn({
        let router = f.router.clone();
        async move {
            request(
                &router,
                "PUT",
                "/api/v1/switch/0/connected",
                "ClientID=9&Connected=true",
            )
            .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), entered.notified())
        .await
        .unwrap();
    // Connected is a lease on the virtual hub; it does not assert every upstream
    // device is online. Unavailable source reads must remain errors.
    assert_eq!(connecting.await.unwrap().1["ErrorNumber"], 0);
    assert_eq!(
        f.call("GET", "/api/v1/switch/0/getswitchvalue", "ClientID=9&Id=1")
            .await["ErrorNumber"],
        0x407
    );
    f.ok(
        "PUT",
        "/api/v1/safetymonitor/0/connected",
        "ClientID=10&Connected=true",
    )
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/0/issafe", "ClientID=10")
            .await,
        false
    );
    release.notify_one();
    eventually(async || {
        f.call("GET", "/api/v1/switch/0/connected", "ClientID=9")
            .await["Value"]
            == true
    })
    .await;
    f.ok(
        "PUT",
        "/api/v1/switch/0/connected",
        "ClientID=9&Connected=false",
    )
    .await;
    eventually(async || {
        f.hub
            .source_snapshot(f.config.sources[0].id)
            .unwrap()
            .lease_count
            == 0
    })
    .await;
    f.finish().await;
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn modern_connection_methods_and_switch_async_contract_preserve_client_ownership() {
    let f = Fixture::new().await;
    for (kind, number, version) in [
        ("switch", 7, 3),
        ("safetymonitor", 3, 3),
        ("observingconditions", 12, 2),
    ] {
        assert_eq!(
            f.ok(
                "GET",
                &format!("/api/v1/{kind}/{number}/interfaceversion"),
                ""
            )
            .await,
            version
        );
        assert_eq!(
            f.ok("GET", &format!("/api/v1/{kind}/{number}/driverversion"), "")
                .await,
            "0.6"
        );
        assert_eq!(
            f.ok(
                "GET",
                &format!("/api/v1/{kind}/{number}/connecting"),
                "ClientID=1"
            )
            .await,
            false
        );
        f.ok(
            "PUT",
            &format!("/api/v1/{kind}/{number}/connect"),
            "ClientID=1",
        )
        .await;
        eventually(async || {
            f.ok(
                "GET",
                &format!("/api/v1/{kind}/{number}/connecting"),
                "ClientID=1",
            )
            .await
                == false
                && f.ok(
                    "GET",
                    &format!("/api/v1/{kind}/{number}/connected"),
                    "ClientID=1",
                )
                .await
                    == true
        })
        .await;
    }
    f.ok("PUT", "/api/v1/switch/7/connect", "ClientID=2").await;
    eventually(async || {
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=2")
            .await
            == true
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/canasync", "ClientID=1&Id=1")
            .await,
        false
    );
    for (method, member, data) in [
        ("GET", "statechangecomplete", "ClientID=1&Id=1"),
        ("PUT", "setasync", "ClientID=1&Id=1&State=true"),
        ("PUT", "setasyncvalue", "ClientID=1&Id=1&Value=30"),
    ] {
        assert_eq!(
            f.call(method, &format!("/api/v1/switch/7/{member}"), data)
                .await["ErrorNumber"],
            0x400
        );
    }
    f.ok("PUT", "/api/v1/switch/7/cancelasync", "ClientID=1&Id=1")
        .await;
    assert_eq!(
        f.call("GET", "/api/v1/switch/7/canasync", "ClientID=1&Id=999")
            .await["ErrorNumber"],
        0x401
    );
    assert_eq!(
        f.call(
            "PUT",
            "/api/v1/switch/7/setasyncvalue",
            "ClientID=1&Id=1&Value=NaN"
        )
        .await["ErrorNumber"],
        0x401
    );
    f.ok("PUT", "/api/v1/switch/7/disconnect", "ClientID=1")
        .await;
    eventually(async || {
        f.ok("GET", "/api/v1/switch/7/connecting", "ClientID=1")
            .await
            == false
    })
    .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=1")
            .await,
        false
    );
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=2")
            .await,
        true
    );
    assert_eq!(
        f.ok("GET", "/api/v1/safetymonitor/3/connected", "ClientID=1")
            .await,
        true
    );
    f.finish().await;
}

#[tokio::test]
async fn asynchronous_open_failure_is_visible_until_explicit_disconnect_and_does_not_leak_capacity()
{
    use regain_hub::client::{Client, ClientLimits};
    let f = Fixture::new().await;
    let endpoint = Endpoint::for_config(&f._dir.path().join("hub.json")).unwrap();
    // Catalog and setup own two distinct streams. Occupy the remainder without
    // output/source leases. Catalog
    // reads still work, but HTTP's new private session is rejected.
    let mut clients = Vec::new();
    for _ in 2..host::MAX_CLIENTS {
        clients.push(
            Client::connect(
                &endpoint,
                f.config.instance_id,
                Duration::from_secs(5),
                ClientLimits::default(),
            )
            .await
            .unwrap(),
        );
    }
    f.ok("PUT", "/api/v1/switch/7/connect", "ClientID=42").await;
    // The accepted attempt owns a real ten-second attachment deadline. A
    // crowded host leaves it pending until that deadline, rather than rejecting
    // its stream immediately. Poll that specific operation to completion.
    tokio::time::timeout(Duration::from_secs(15), async {
        while f
            .call("GET", "/api/v1/switch/7/connecting", "ClientID=42")
            .await["ErrorNumber"]
            == 0
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let failure = f
        .call("GET", "/api/v1/switch/7/connecting", "ClientID=42")
        .await;
    assert_ne!(failure["ErrorNumber"], 0);
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connected", "ClientID=42")
            .await,
        false
    );
    assert_eq!(
        f.call("GET", "/api/v1/switch/7/connecting", "ClientID=42")
            .await["ErrorNumber"],
        failure["ErrorNumber"]
    );
    f.ok("PUT", "/api/v1/switch/7/disconnect", "ClientID=42")
        .await;
    assert_eq!(
        f.ok("GET", "/api/v1/switch/7/connecting", "ClientID=42")
            .await,
        false
    );
    drop(clients);
    for id in 1..=26 {
        f.ok("PUT", "/api/v1/switch/7/connect", &format!("ClientID={id}"))
            .await;
        eventually(async || {
            f.ok(
                "GET",
                "/api/v1/switch/7/connected",
                &format!("ClientID={id}"),
            )
            .await
                == true
                && f.ok(
                    "GET",
                    "/api/v1/switch/7/connecting",
                    &format!("ClientID={id}"),
                )
                .await
                    == false
        })
        .await;
        f.ok(
            "PUT",
            "/api/v1/switch/7/disconnect",
            &format!("ClientID={id}"),
        )
        .await;
        eventually(async || {
            f.ok(
                "GET",
                "/api/v1/switch/7/connecting",
                &format!("ClientID={id}"),
            )
            .await
                == false
        })
        .await;
    }
    eventually(async || f.hub.active_connections() == 0).await;
    f.finish().await;
}
