//! Private typed actors and loopback transports; no physical motion.
#[path = "support/rotator_runtime.rs"]
mod runtime;
use regain_hub::{
    parameters::PollPolicy,
    rotator::{RotatorController, RotatorProperty},
    safety::MonotonicClock,
    source::{Backend, BackendFuture, ErrorKind, SourceError, SourceHandle, Values},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};
use uuid::Uuid;

struct Device {
    values: Mutex<Values>,
    errors: Mutex<std::collections::BTreeMap<String, SourceError>>,
    writes: Mutex<Vec<(String, Values)>>,
    connects: AtomicUsize,
    reads: AtomicUsize,
    polls: AtomicUsize,
    disconnects: AtomicUsize,
    pending: AtomicBool,
    uncertain: AtomicBool,
    hang_write: AtomicBool,
    hang_read: Mutex<Option<String>>,
}
impl Device {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            values: Mutex::new(Values::from([
                ("canreverse".into(), json!(true)),
                ("ismoving".into(), json!(false)),
                ("mechanicalposition".into(), json!(350.0)),
                ("position".into(), json!(20.0)),
                ("reverse".into(), json!(false)),
                ("stepsize".into(), json!(0.02)),
                ("targetposition".into(), json!(20.0)),
            ])),
            errors: Mutex::default(),
            writes: Mutex::default(),
            connects: AtomicUsize::new(0),
            reads: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
            pending: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
            hang_write: AtomicBool::new(false),
            hang_read: Mutex::default(),
        })
    }
    fn set(&self, member: &str, value: Value) {
        self.values.lock().unwrap().insert(member.into(), value);
    }
    fn command(&self, member: String, args: Values) {
        self.writes
            .lock()
            .unwrap()
            .push((member.clone(), args.clone()));
        match member.as_str() {
            "move" => {
                let current = self.values.lock().unwrap()["position"].as_f64().unwrap();
                self.set(
                    "targetposition",
                    json!((current + args["Position"].as_f64().unwrap()).rem_euclid(360.0)),
                );
                self.set("ismoving", json!(true));
            }
            "moveabsolute" => {
                self.set("targetposition", args["Position"].clone());
                self.set("ismoving", json!(true));
            }
            "movemechanical" => self.set("ismoving", json!(true)),
            "sync" => self.set("position", args["Position"].clone()),
            "reverse" => self.set("reverse", args["Reverse"].clone()),
            "halt" => self.set("ismoving", json!(false)),
            _ => panic!("Unexpected command {member}"),
        }
    }
}
struct Mock(Arc<Device>);
impl Backend for Mock {
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.0.connects.fetch_add(1, SeqCst);
            Ok(())
        })
    }
    fn connect_step(&mut self) -> BackendFuture<'_, bool> {
        Box::pin(async {
            if self.0.pending.load(SeqCst) {
                Ok(false)
            } else {
                self.connect().await.map(|()| true)
            }
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.0.disconnects.fetch_add(1, SeqCst);
            Ok(())
        })
    }
    fn read(&mut self, member: String, args: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.0.reads.fetch_add(1, SeqCst);
            assert!(args.is_empty());
            let hang = self.0.hang_read.lock().unwrap().as_ref() == Some(&member);
            if hang {
                std::future::pending::<()>().await;
            }
            if let Some(error) = self.0.errors.lock().unwrap().get(&member) {
                return Err(error.clone());
            }
            Ok(self.0.values.lock().unwrap()[&member].clone())
        })
    }
    fn write(&mut self, member: String, args: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            if let Some(error) = self.0.errors.lock().unwrap().get(&member) {
                return Err(error.clone());
            }
            self.0.command(member, args);
            if self.0.hang_write.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.0.uncertain.load(SeqCst) {
                return Err(SourceError::uncertain());
            }
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async {
            self.0.polls.fetch_add(1, SeqCst);
            Ok(self.0.values.lock().unwrap().clone())
        })
    }
    fn sample(&mut self) -> BackendFuture<'_, regain_hub::source::SampleBatch> {
        Box::pin(async {
            let mut batch = regain_hub::source::SampleBatch::from(self.poll().await?);
            batch.errors = self.0.errors.lock().unwrap().clone();
            for key in batch.errors.keys() {
                batch.values.remove(key);
            }
            Ok(batch)
        })
    }
    fn reset(&mut self) {}
}
fn setup(device: &Arc<Device>) -> (Arc<SourceHandle>, RotatorController) {
    let source = SourceHandle::spawn(
        Uuid::new_v4(),
        Uuid::new_v4(),
        PollPolicy {
            request_timeout_seconds: 0.1,
            poll_seconds: 1.0,
            ..PollPolicy::default()
        },
        Box::new(Mock(device.clone())),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let controller = RotatorController::new(source.clone(), Duration::from_secs(2)).unwrap();
    (source, controller)
}
async fn settle() {
    for _ in 0..24 {
        tokio::task::yield_now().await;
    }
}
#[tokio::test(start_paused = true)]
async fn shared_clients_keep_separate_angles_and_acknowledge_start_without_automatic_halt() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    assert_eq!(device.connects.load(SeqCst), 0);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    assert_eq!(device.connects.load(SeqCst), 1);
    assert_eq!(first.generation(), second.generation());
    assert_eq!(
        first
            .property(RotatorProperty::MechanicalPosition)
            .await
            .unwrap(),
        350.0
    );
    assert_eq!(
        first.property(RotatorProperty::Position).await.unwrap(),
        20.0
    );
    first.move_relative(-721.5).await.unwrap();
    assert!(second.is_moving().await.unwrap());
    assert_eq!(
        second
            .property(RotatorProperty::TargetPosition)
            .await
            .unwrap(),
        18.5
    );
    assert_eq!(
        second.property(RotatorProperty::Position).await.unwrap(),
        20.0
    );
    assert_eq!(
        second.move_absolute(30.0).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    drop(first);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.disconnects.load(SeqCst), 0);
    assert!(second.is_moving().await.unwrap());
    second.halt().await.unwrap();
    second.sync(42.5).await.unwrap();
    assert_eq!(
        second
            .property(RotatorProperty::MechanicalPosition)
            .await
            .unwrap(),
        350.0
    );
    second.set_reverse(true).await.unwrap();
    second.move_mechanical(12.25).await.unwrap();
    let writes = device.writes.lock().unwrap().clone();
    assert_eq!(writes.len(), 5);
    assert_eq!(
        writes[0],
        (
            "move".into(),
            Values::from([("Position".into(), json!(-721.5))])
        )
    );
    assert_eq!(
        writes[4],
        (
            "movemechanical".into(),
            Values::from([("Position".into(), json!(12.25))])
        )
    );
    drop(second);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    assert_eq!(device.disconnects.load(SeqCst), 1);
    assert_eq!(device.writes.lock().unwrap().len(), 5);
    source.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn invalid_angles_and_malformed_motion_do_not_dispatch_or_become_idle() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    for value in [
        -0.01,
        360.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::MAX,
    ] {
        assert_eq!(
            session.move_absolute(value).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            session.move_mechanical(value).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            session.sync(value).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
    for value in [f64::NAN, f64::INFINITY, f64::MAX] {
        assert_eq!(
            session.move_relative(value).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
    for value in [json!("false"), json!(0), Value::Null] {
        device.set("ismoving", value);
        assert_eq!(
            session.is_moving().await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            session.move_absolute(0.0).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            session.sync(0.0).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            session.set_reverse(true).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
    }
    assert!(device.writes.lock().unwrap().is_empty());
    source.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn strict_properties_optional_errors_and_live_reverse_capability_are_preserved() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    for property in [
        RotatorProperty::Position,
        RotatorProperty::MechanicalPosition,
        RotatorProperty::TargetPosition,
    ] {
        for value in [json!(-1), json!(360), json!("30"), Value::Null] {
            device.set(property.member(), value);
            assert_eq!(
                session.property(property).await.unwrap_err().kind,
                ErrorKind::Unavailable
            );
        }
    }
    device.set("stepsize", json!(0));
    assert_eq!(
        session
            .property(RotatorProperty::StepSize)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    device.errors.lock().unwrap().insert(
        "stepsize".into(),
        SourceError {
            upstream_code: Some(1024),
            ..SourceError::new(ErrorKind::Unsupported, "Optional property is unavailable")
        },
    );
    assert_eq!(
        session
            .property(RotatorProperty::StepSize)
            .await
            .unwrap_err()
            .upstream_code,
        Some(1024)
    );
    device.set("canreverse", json!(false));
    assert!(!session.capabilities().await.unwrap().can_reverse);
    assert_eq!(
        session.set_reverse(true).await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    device.errors.lock().unwrap().insert(
        "halt".into(),
        SourceError {
            upstream_code: Some(1024),
            ..SourceError::new(ErrorKind::Unsupported, "Optional command is unavailable")
        },
    );
    assert_eq!(session.halt().await.unwrap_err().upstream_code, Some(1024));
    device.set("canreverse", json!(true));
    session.set_reverse(true).await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn capability_failure_pending_connection_and_timeout_release_only_own_lease() {
    let device = Device::new();
    device.set("canreverse", json!(1));
    let (source, controller) = setup(&device);
    assert_eq!(
        controller.connect().await.err().unwrap().kind,
        ErrorKind::Unavailable
    );
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    device.set("canreverse", json!(true));
    device.pending.store(true, SeqCst);
    let connect = tokio::spawn(async move { controller.connect().await });
    settle().await;
    assert_eq!(device.connects.load(SeqCst), 1);
    connect.abort();
    assert!(connect.await.err().unwrap().is_cancelled());
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    device.pending.store(false, SeqCst);
    *device.hang_read.lock().unwrap() = Some("canreverse".into());
    let controller = RotatorController::new(source.clone(), Duration::from_secs(2)).unwrap();
    assert!(controller.connect().await.is_err());
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    assert!(device.writes.lock().unwrap().is_empty());
    source.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn uncertain_applied_mutation_fences_all_clients_without_replay_or_auto_halt() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    device.uncertain.store(true, SeqCst);
    assert_eq!(
        first.move_absolute(25.0).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    device.uncertain.store(false, SeqCst);
    assert!(!first.connected());
    assert!(!second.connected());
    assert_eq!(second.halt().await.unwrap_err().kind, ErrorKind::Uncertain);
    assert_eq!(
        second.sync(12.0).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        second.set_reverse(true).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    assert_eq!(device.values.lock().unwrap()["targetposition"], 25.0);
    assert!(source.snapshot().write_uncertain);
    drop(first);
    drop(second);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    device.set("ismoving", json!(false));
    let fresh = controller.connect().await.unwrap();
    fresh.sync(12.0).await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    source.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn generation_loss_in_preflight_blocks_dispatch_and_never_adopts_reconnection() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    let generation = session.generation();
    *device.hang_read.lock().unwrap() = Some("ismoving".into());
    assert_eq!(
        session.move_absolute(25.0).await.unwrap_err().kind,
        ErrorKind::Transient
    );
    assert_ne!(source.snapshot().generation, generation);
    assert!(!session.connected());
    assert!(device.writes.lock().unwrap().is_empty());
    *device.hang_read.lock().unwrap() = None;
    let fresh = controller.connect().await.unwrap();
    assert_ne!(fresh.generation(), generation);
    assert_eq!(
        session.halt().await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    fresh.move_absolute(25.0).await.unwrap();
    source.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn cancelled_preflight_and_dispatched_move_have_distinct_uncertainty() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = Arc::new(controller.connect().await.unwrap());
    *device.hang_read.lock().unwrap() = Some("ismoving".into());
    let task = tokio::spawn({
        let session = session.clone();
        async move { session.move_absolute(25.0).await }
    });
    settle().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    *device.hang_read.lock().unwrap() = None;
    tokio::time::advance(Duration::from_millis(100)).await;
    settle().await;
    assert!(device.writes.lock().unwrap().is_empty());
    assert!(!source.snapshot().write_uncertain);
    let fresh = Arc::new(controller.connect().await.unwrap());
    device.hang_write.store(true, SeqCst);
    let task = tokio::spawn({
        let fresh = fresh.clone();
        async move { fresh.move_absolute(25.0).await }
    });
    settle().await;
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::advance(Duration::from_millis(100)).await;
    settle().await;
    assert!(source.snapshot().write_uncertain);
    assert_eq!(fresh.halt().await.unwrap_err().kind, ErrorKind::Uncertain);
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn concurrent_calls_from_one_session_admit_only_one_motion_command() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    let (a, b) = tokio::join!(session.move_absolute(25.0), session.move_absolute(30.0));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(a.err().or(b.err()).unwrap().kind, ErrorKind::Busy);
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}

// Exercise the actual transport, including V3 legacy Connected and V4 async
// Connect/Connecting/Disconnect. These servers implement only private state.
async fn alpaca_transport(version: u16, uncertain_reply: bool) {
    use axum::{
        Json, Router,
        body::to_bytes,
        extract::{Request, State},
        routing::any,
    };
    use regain_hub::{
        config::{
            ConnectionPolicy, DeviceType, HubConfig, OutputConfig, SourceBackend, SourceConfig,
            VirtualDevice,
        },
        factory::{NoCredentials, build_sources},
        native::NativeRuntime,
        runtime::HubRuntime,
        source::ConnectionMethod,
    };
    #[derive(Clone)]
    struct HttpDevice {
        device: Arc<Device>,
        connected: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<(String, String, Values)>>>,
        version: u16,
        uncertain_reply: bool,
    }
    async fn handler(State(state): State<HttpDevice>, request: Request) -> Json<Value> {
        assert!(
            request
                .uri()
                .path()
                .starts_with("/prefix/api/v1/rotator/7/")
        );
        let member = request.uri().path().rsplit('/').next().unwrap().to_owned();
        let method = request.method().as_str().to_owned();
        let data = if method == "GET" {
            request
                .uri()
                .query()
                .unwrap_or_default()
                .as_bytes()
                .to_vec()
        } else {
            to_bytes(request.into_body(), 4096).await.unwrap().to_vec()
        };
        let parameters = url::form_urlencoded::parse(&data)
            .map(|(key, value)| (key.into_owned(), json!(value)))
            .collect::<Values>();
        for key in ["ClientID", "ClientTransactionID"] {
            assert!(parameters[key].as_str().unwrap().parse::<u32>().unwrap() > 0);
        }
        state
            .requests
            .lock()
            .unwrap()
            .push((method.clone(), member.clone(), parameters.clone()));
        let value = if method == "PUT" {
            match member.as_str() {
                "connected" => {
                    assert_eq!(state.version, 3);
                    state
                        .connected
                        .store(parameters["Connected"] == "true", SeqCst);
                }
                "connect" | "disconnect" => {
                    assert_eq!(state.version, 4);
                    assert_eq!(parameters.len(), 2);
                    state.connected.store(member == "connect", SeqCst);
                }
                "halt" => {
                    return Json(json!({"ErrorNumber":1024,"ErrorMessage":"private halt detail"}));
                }
                _ => {
                    assert!(state.connected.load(SeqCst));
                    let args = match member.as_str() {
                        "move" | "moveabsolute" | "movemechanical" | "sync" => Values::from([(
                            "Position".into(),
                            json!(
                                parameters["Position"]
                                    .as_str()
                                    .unwrap()
                                    .parse::<f64>()
                                    .unwrap()
                            ),
                        )]),
                        "reverse" => Values::from([(
                            "Reverse".into(),
                            json!(parameters["Reverse"] == "true"),
                        )]),
                        _ => panic!("Unexpected rotator HTTP write {member}"),
                    };
                    state.device.command(member.clone(), args);
                    if state.uncertain_reply {
                        // Applied mutation, but a malformed acknowledgment. It
                        // must not turn into a successful/replayed command.
                        return Json(json!({"ErrorNumber":"lost acknowledgment"}));
                    }
                }
            }
            Value::Null
        } else {
            match member.as_str() {
                "interfaceversion" => json!(state.version),
                "connected" => json!(state.connected.load(SeqCst)),
                "connecting" => {
                    assert_eq!(state.version, 4);
                    json!(false)
                }
                "stepsize" => {
                    return Json(
                        json!({"ErrorNumber":1024,"ErrorMessage":"private property detail"}),
                    );
                }
                _ => state.device.values.lock().unwrap()[&member].clone(),
            }
        };
        Json(
            json!({"ErrorNumber":0,"Value":value,"ClientTransactionID":parameters["ClientTransactionID"].as_str().unwrap().parse::<u32>().unwrap()}),
        )
    }
    let fixture = HttpDevice {
        device: Device::new(),
        connected: Arc::new(AtomicBool::new(false)),
        requests: Arc::new(Mutex::new(Vec::new())),
        version,
        uncertain_reply,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new()
        .fallback(any(handler))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let config = SourceConfig {
        id: Uuid::new_v4(),
        label: "Private HTTP rotator".into(),
        polling: PollPolicy {
            request_timeout_seconds: 1.0,
            poll_seconds: 1.0,
            ..PollPolicy::default()
        },
        backend: SourceBackend::Alpaca {
            base_url: format!("http://{address}/prefix/"),
            device_type: DeviceType::Rotator,
            device_number: 7,
            connection_policy: ConnectionPolicy::Managed,
            credential_reference: None,
        },
    };
    let mut hub_config = HubConfig::empty();
    hub_config.sources.push(config.clone());
    let output = Uuid::new_v4();
    hub_config.outputs.push(OutputConfig {
        id: output,
        number: 23,
        label: "HTTP rotator output".into(),
        device: VirtualDevice::Proxy {
            source: config.id,
            device_type: DeviceType::Rotator,
        },
    });
    let clock = Arc::new(MonotonicClock::default());
    let directory = tempfile::tempdir().unwrap();
    let registry = build_sources(
        &hub_config,
        &NativeRuntime {
            directory: directory.path().into(),
            simulate: false,
            references: None,
        },
        &NoCredentials,
        clock.clone(),
    )
    .unwrap();
    let source = registry.get(config.id).unwrap();
    let host = HubRuntime::from_registry(hub_config, registry, clock).unwrap();
    let first_client = host.client();
    let second_client = host.client();
    first_client.connect(output).await.unwrap();
    second_client.connect(output).await.unwrap();
    let first_connection = first_client.connection(output).unwrap();
    let second_connection = second_client.connection(output).unwrap();
    let first = first_connection.rotator().unwrap();
    let second = second_connection.rotator().unwrap();
    assert_eq!(first.generation(), second.generation());
    let info = source.snapshot().connection_info.unwrap();
    assert_eq!(info.interface_version, Some(version));
    assert_eq!(
        info.method,
        if version == 4 {
            ConnectionMethod::Async
        } else {
            ConnectionMethod::Legacy
        }
    );
    assert!(info.owns_connection);
    if uncertain_reply {
        assert_eq!(
            first.move_absolute(25.0).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            fixture.device.values.lock().unwrap()["targetposition"],
            25.0
        );
        assert!(!second.connected());
        assert_eq!(second.halt().await.unwrap_err().kind, ErrorKind::Uncertain);
        assert_eq!(
            second.sync(12.0).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(fixture.device.writes.lock().unwrap().len(), 1);
    } else {
        let error = first.property(RotatorProperty::StepSize).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert_eq!(error.upstream_code, Some(1024));
        assert!(!error.message.contains("private"));
        assert_eq!(
            first.move_absolute(360.0).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        fixture.device.set("ismoving", json!("false"));
        assert_eq!(
            first.move_absolute(25.0).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert!(fixture.device.writes.lock().unwrap().is_empty());
        fixture.device.set("ismoving", json!(false));
        first.move_relative(-721.5).await.unwrap();
        assert!(second.is_moving().await.unwrap());
        assert_eq!(
            second
                .property(RotatorProperty::TargetPosition)
                .await
                .unwrap(),
            18.5
        );
        assert_eq!(
            first.property(RotatorProperty::Position).await.unwrap(),
            20.0
        );
        assert_eq!(second.sync(40.0).await.unwrap_err().kind, ErrorKind::Busy);
        assert_eq!(second.halt().await.unwrap_err().upstream_code, Some(1024));
        fixture.device.set("ismoving", json!(false));
        second.sync(42.5).await.unwrap();
        assert_eq!(
            first
                .property(RotatorProperty::MechanicalPosition)
                .await
                .unwrap(),
            350.0
        );
        second.set_reverse(true).await.unwrap();
        assert_eq!(
            first.property(RotatorProperty::Reverse).await.unwrap(),
            true
        );
        second.move_mechanical(12.25).await.unwrap();
        fixture.device.set("ismoving", json!(false));
        first.move_absolute(30.0).await.unwrap();
        let writes = fixture.device.writes.lock().unwrap().clone();
        assert_eq!(writes.len(), 5);
        assert_eq!(writes[0].1["Position"], -721.5);
        assert_eq!(writes[1].0, "sync");
        assert_eq!(writes[1].1["Position"], 42.5);
        assert_eq!(writes[2].1["Reverse"], true);
        assert_eq!(writes[3].0, "movemechanical");
        assert_eq!(writes[4].0, "moveabsolute");
    }
    drop(first_connection);
    first_client.close();
    // A FIFO read from the surviving output is stronger than a scheduler yield
    // for proving another output's Drop cannot disconnect this client.
    if !uncertain_reply {
        assert!(second.is_moving().await.unwrap());
    }
    assert!(fixture.connected.load(SeqCst));
    drop(second_connection);
    second_client.close();
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(5), async {
        while status.borrow_and_update().lease_count != 0 {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!fixture.connected.load(SeqCst));
    let requests = fixture.requests.lock().unwrap().clone();
    let connection_writes = requests
        .iter()
        .filter(|(method, member, _)| {
            method == "PUT" && matches!(member.as_str(), "connect" | "disconnect" | "connected")
        })
        .collect::<Vec<_>>();
    assert_eq!(connection_writes.len(), 2);
    assert_eq!(
        connection_writes[0].1,
        if version == 4 { "connect" } else { "connected" }
    );
    assert_eq!(
        connection_writes[1].1,
        if version == 4 {
            "disconnect"
        } else {
            "connected"
        }
    );
    let command_writes = requests
        .iter()
        .filter(|(method, member, _)| {
            method == "PUT" && !matches!(member.as_str(), "connect" | "disconnect" | "connected")
        })
        .collect::<Vec<_>>();
    assert_eq!(command_writes.len(), if uncertain_reply { 1 } else { 6 });
    assert_eq!(
        command_writes[0].2["Position"],
        if uncertain_reply { "25.0" } else { "-721.5" }
    );
    let client = &requests[0].2["ClientID"];
    assert!(
        requests
            .iter()
            .all(|(_, _, args)| &args["ClientID"] == client)
    );
    let transactions = requests
        .iter()
        .map(|(_, _, args)| args["ClientTransactionID"].as_str().unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(transactions.len(), requests.len());
    host.shutdown().await.unwrap();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
#[tokio::test]
async fn actual_alpaca_v3_transport_preserves_rotator_parameters_and_errors() {
    alpaca_transport(3, false).await;
}
#[tokio::test]
async fn actual_alpaca_v4_transport_negotiates_shared_async_connection() {
    alpaca_transport(4, false).await;
}
#[tokio::test]
async fn actual_alpaca_applied_move_with_malformed_acknowledgment_is_not_replayed() {
    alpaca_transport(4, true).await;
}
