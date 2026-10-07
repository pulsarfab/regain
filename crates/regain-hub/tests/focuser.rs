//! Typed proxy faults are private actors, never physical motion.
use regain_hub::{
    coordination::{
        FocuserCalibration, FocuserGroup, FocuserGroupConfig, FocuserGroupPhase, FocuserMemberPhase,
    },
    focuser::FocuserController,
    parameters::PollPolicy,
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
    pending: AtomicBool,
    hang_read: Mutex<Option<String>>,
    reads: AtomicUsize,
    polls: AtomicUsize,
    connects: AtomicUsize,
    disconnects: AtomicUsize,
    uncertain: AtomicBool,
    hang_write: AtomicBool,
    write_error: Mutex<Option<SourceError>>,
    write_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    written: tokio::sync::Notify,
    ignore_move: AtomicBool,
}
impl Device {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            values: Mutex::new(Values::from([
                ("absolute".into(), json!(true)),
                ("maxstep".into(), json!(1000)),
                ("maxincrement".into(), json!(1000)),
                ("tempcompavailable".into(), json!(true)),
                ("position".into(), json!(50)),
                ("ismoving".into(), json!(false)),
                ("tempcomp".into(), json!(true)),
                ("temperature".into(), json!(-5.0)),
                ("stepsize".into(), json!(1.25)),
            ])),
            errors: Mutex::default(),
            writes: Mutex::default(),
            pending: AtomicBool::new(false),
            hang_read: Mutex::default(),
            reads: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            connects: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
            uncertain: AtomicBool::new(false),
            hang_write: AtomicBool::new(false),
            write_error: Mutex::default(),
            write_gate: Mutex::default(),
            written: tokio::sync::Notify::new(),
            ignore_move: AtomicBool::new(false),
        })
    }
    fn set(&self, member: &str, value: Value) {
        self.values.lock().unwrap().insert(member.into(), value);
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
    fn read(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            assert!(parameters.is_empty());
            self.0.reads.fetch_add(1, SeqCst);
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
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.0
                .writes
                .lock()
                .unwrap()
                .push((member.clone(), parameters.clone()));
            self.0.written.notify_one();
            let gate = self.0.write_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
            if self.0.hang_write.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.0.uncertain.load(SeqCst) {
                return Err(SourceError::uncertain());
            }
            if let Some(error) = self.0.write_error.lock().unwrap().clone() {
                return Err(error);
            }
            match member.as_str() {
                "move" => {
                    if !self.0.ignore_move.load(SeqCst) {
                        self.0.set("position", parameters["Position"].clone());
                        self.0.set("ismoving", json!(true));
                    }
                }
                "halt" => self.0.set("ismoving", json!(false)),
                "tempcomp" => self.0.set("tempcomp", parameters["TempComp"].clone()),
                _ => panic!("Unexpected command {member}"),
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
    fn reset(&mut self) {}
}
fn setup(device: &Arc<Device>) -> (Arc<SourceHandle>, FocuserController) {
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
    let controller = FocuserController::new(source.clone(), Duration::from_secs(2)).unwrap();
    (source, controller)
}
async fn settle() {
    for _ in 0..24 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn shared_clients_observe_motion_and_only_last_disconnect_releases_source() {
    let device = Device::new();
    device.set("position", json!(780));
    device.set("maxincrement", json!(20));
    let (source, controller) = setup(&device);
    assert_eq!(device.connects.load(SeqCst), 0);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    assert_eq!(device.connects.load(SeqCst), 1);
    assert_eq!(first.generation(), second.generation());
    assert_eq!(source.snapshot().lease_count, 2);
    assert_eq!(first.temperature().await.unwrap(), -5.0);
    assert_eq!(first.step_size().await.unwrap(), 1.25);
    assert_eq!(first.position().await.unwrap(), 780);
    // Absolute targets use MaxStep, not the relative MaxIncrement.
    first.move_to(800).await.unwrap();
    assert!(second.is_moving().await.unwrap());
    assert!(second.temp_comp().await.unwrap());
    assert_eq!(second.move_to(700).await.unwrap_err().kind, ErrorKind::Busy);
    second.halt().await.unwrap();
    assert!(!first.is_moving().await.unwrap());
    assert_eq!(first.position().await.unwrap(), 800);
    second.set_temp_comp(false).await.unwrap();
    assert!(!first.temp_comp().await.unwrap());
    assert_eq!(device.writes.lock().unwrap().len(), 3);
    drop(first);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.disconnects.load(SeqCst), 0);
    drop(second);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    assert_eq!(device.disconnects.load(SeqCst), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn relative_moves_preserve_signed_distance_and_reject_overflow_and_absolute_position() {
    let device = Device::new();
    device.set("absolute", json!(false));
    device.set("maxincrement", json!(20));
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    assert_eq!(
        session.position().await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    for value in [-21, 21, i32::MIN, i32::MAX] {
        assert_eq!(
            session.move_to(value).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
    session.move_to(-20).await.unwrap();
    session.halt().await.unwrap();
    session.move_to(20).await.unwrap();
    let writes = device.writes.lock().unwrap().clone();
    assert_eq!(writes[0].1["Position"], -20);
    assert_eq!(writes[2].1["Position"], 20);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn live_limits_optional_errors_and_strict_types_are_not_fabricated() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    device.set("maxstep", json!(60));
    for value in [-1, 61, i32::MAX] {
        assert_eq!(
            session.move_to(value).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
    device.set("tempcompavailable", json!(false));
    assert_eq!(
        session.set_temp_comp(false).await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    device.errors.lock().unwrap().insert(
        "stepsize".into(),
        SourceError {
            upstream_code: Some(1024),
            ..SourceError::new(ErrorKind::Unsupported, "Optional property unavailable")
        },
    );
    let error = session.step_size().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(error.upstream_code, Some(1024));
    device.set("ismoving", json!(1));
    assert_eq!(
        session.move_to(60).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    for (member, value) in [
        ("absolute", json!(1)),
        ("maxstep", json!(1.5)),
        ("maxincrement", json!(0)),
        ("tempcompavailable", json!("false")),
    ] {
        let original = device.values.lock().unwrap()[member].clone();
        device.set(member, value);
        assert_eq!(
            session.capabilities().await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        device.set(member, original);
    }
    assert!(device.writes.lock().unwrap().is_empty());
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn pending_connection_waits_for_actual_generation_and_cancelled_connect_releases_lease() {
    let device = Device::new();
    device.pending.store(true, SeqCst);
    let (source, controller) = setup(&device);
    let controller = Arc::new(controller);
    let connect = tokio::spawn({
        let controller = controller.clone();
        async move { controller.connect().await }
    });
    settle().await;
    assert!(!connect.is_finished());
    assert_eq!(device.reads.load(SeqCst), 0);
    device.pending.store(false, SeqCst);
    tokio::time::advance(Duration::from_millis(100)).await;
    let session = connect.await.unwrap().unwrap();
    assert_eq!(session.generation(), source.snapshot().generation);
    drop(session);
    settle().await;
    device.pending.store(true, SeqCst);
    let connect = tokio::spawn({
        let controller = controller.clone();
        async move { controller.connect().await }
    });
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    connect.abort();
    assert!(matches!(connect.await, Err(error) if error.is_cancelled()));
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn stalled_connection_and_invalid_capability_release_only_their_own_lease() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let first = controller.connect().await.unwrap();
    device.set("absolute", json!(null));
    assert!(
        matches!(controller.connect().await, Err(error) if error.kind == ErrorKind::Unavailable)
    );
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    drop(first);
    settle().await;
    device.pending.store(true, SeqCst);
    assert!(
        matches!(controller.connect().await, Err(error) if error.kind == ErrorKind::Unavailable)
    );
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn preflight_transport_loss_prevents_move_and_old_session_cannot_adopt_reconnection() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    let generation = session.generation();
    *device.hang_read.lock().unwrap() = Some("maxincrement".into());
    assert_eq!(
        session.move_to(100).await.unwrap_err().kind,
        ErrorKind::Transient
    );
    assert!(device.writes.lock().unwrap().is_empty());
    assert_ne!(source.snapshot().generation, generation);
    *device.hang_read.lock().unwrap() = None;
    let replacement = controller.connect().await.unwrap();
    assert_ne!(replacement.generation(), generation);
    assert_eq!(
        session.move_to(100).await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    replacement.move_to(100).await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn dispatched_uncertainty_blocks_all_clients_without_replay_or_automatic_halt() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    device.uncertain.store(true, SeqCst);
    assert_eq!(
        first.move_to(100).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        second.move_to(200).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(second.halt().await.unwrap_err().kind, ErrorKind::Uncertain);
    tokio::time::advance(Duration::from_secs(5)).await;
    settle().await;
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    assert!(source.snapshot().write_uncertain);
    drop(first);
    settle().await;
    assert!(source.snapshot().write_uncertain);
    drop(second);
    settle().await;
    assert!(!source.snapshot().write_uncertain);
    device.uncertain.store(false, SeqCst);
    let fresh = controller.connect().await.unwrap();
    fresh.move_to(200).await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_preflight_releases_control_without_dispatching_motion() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = Arc::new(controller.connect().await.unwrap());
    *device.hang_read.lock().unwrap() = Some("maxstep".into());
    let command = tokio::spawn({
        let session = session.clone();
        async move { session.move_to(100).await }
    });
    settle().await;
    command.abort();
    assert!(command.await.unwrap_err().is_cancelled());
    *device.hang_read.lock().unwrap() = None;
    tokio::time::advance(Duration::from_millis(100)).await;
    settle().await;
    assert!(device.writes.lock().unwrap().is_empty());
    assert_eq!(source.snapshot().lease_count, 1);
    let fresh = controller.connect().await.unwrap();
    fresh.move_to(100).await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn another_operation_owns_control_until_its_lease_is_released() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = Arc::new(controller.connect().await.unwrap());
    // Another admitted operation owns control while this command is prepared.
    let operation = regain_hub::readout::SourceLease::acquire(source.clone())
        .await
        .unwrap();
    source.control(operation.id, true).await.unwrap();
    assert_eq!(
        session.move_to(100).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert!(device.writes.lock().unwrap().is_empty());
    drop(operation);
    settle().await;
    session.move_to(100).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_dispatched_move_retains_uncertainty_after_backend_deadline() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = Arc::new(controller.connect().await.unwrap());
    device.hang_write.store(true, SeqCst);
    let command = tokio::spawn({
        let session = session.clone();
        async move { session.move_to(100).await }
    });
    settle().await;
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    command.abort();
    assert!(command.await.unwrap_err().is_cancelled());
    tokio::time::advance(Duration::from_millis(100)).await;
    settle().await;
    assert!(source.snapshot().write_uncertain);
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(
        session.move_to(100).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn absolute_target_and_per_move_travel_are_separate_limits() {
    let device = Device::new();
    device.set("position", json!(780));
    device.set("maxincrement", json!(20));
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    for target in [759, 801, 1001, -1] {
        assert_eq!(
            session.move_to(target).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
    assert!(device.writes.lock().unwrap().is_empty());
    session.move_to(800).await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn concurrent_calls_from_one_session_admit_only_one_move() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    let (first, second) = tokio::join!(session.move_to(100), session.move_to(200));
    assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
    let error = first.err().or(second.err()).unwrap();
    assert_eq!(error.kind, ErrorKind::Busy);
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test]
async fn actual_alpaca_transport_preserves_shared_connection_command_parameters_and_errors() {
    use axum::{
        Json, Router,
        body::to_bytes,
        extract::{Request, State},
        routing::any,
    };
    use regain_hub::{
        alpaca::AlpacaBackend,
        config::{ConnectionPolicy, DeviceType, SourceBackend, SourceConfig},
    };
    #[derive(Clone)]
    struct HttpDevice {
        device: Arc<Device>,
        connected: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<(String, String, Values)>>>,
    }
    async fn handler(State(state): State<HttpDevice>, request: Request) -> Json<Value> {
        assert!(
            request
                .uri()
                .path()
                .starts_with("/prefix/api/v1/focuser/7/")
        );
        let member = request.uri().path().rsplit('/').next().unwrap().to_owned();
        let method = request.method().as_str().to_owned();
        let data = if method == "GET" {
            request
                .uri()
                .query()
                .unwrap_or_default()
                .to_owned()
                .into_bytes()
        } else {
            to_bytes(request.into_body(), 4096).await.unwrap().to_vec()
        };
        let parameters = url::form_urlencoded::parse(&data)
            .map(|(key, value)| (key.into_owned(), json!(value)))
            .collect::<Values>();
        assert!(
            parameters["ClientID"]
                .as_str()
                .unwrap()
                .parse::<u32>()
                .unwrap()
                > 0
        );
        assert!(
            parameters["ClientTransactionID"]
                .as_str()
                .unwrap()
                .parse::<u32>()
                .unwrap()
                > 0
        );
        state
            .requests
            .lock()
            .unwrap()
            .push((method.clone(), member.clone(), parameters.clone()));
        let value = if method == "PUT" {
            if member == "connected" {
                state
                    .connected
                    .store(parameters["Connected"] == "true", SeqCst);
            } else {
                let args = match member.as_str() {
                    "move" => Values::from([(
                        "Position".into(),
                        json!(
                            parameters["Position"]
                                .as_str()
                                .unwrap()
                                .parse::<i32>()
                                .unwrap()
                        ),
                    )]),
                    "tempcomp" => {
                        Values::from([("TempComp".into(), json!(parameters["TempComp"] == "true"))])
                    }
                    "halt" => Values::new(),
                    _ => panic!("Unexpected HTTP write {member}"),
                };
                Mock(state.device.clone())
                    .write(member.clone(), args)
                    .await
                    .unwrap();
            }
            Value::Null
        } else {
            match member.as_str() {
                "interfaceversion" => json!(3),
                "connected" => json!(state.connected.load(SeqCst)),
                "stepsize" => {
                    return Json(
                        json!({"ErrorNumber":1024, "ErrorMessage":"private upstream detail"}),
                    );
                }
                _ => state.device.values.lock().unwrap()[&member].clone(),
            }
        };
        Json(json!({"ErrorNumber":0, "Value":value}))
    }
    let fixture = HttpDevice {
        device: Device::new(),
        connected: Arc::new(AtomicBool::new(false)),
        requests: Arc::new(Mutex::new(Vec::new())),
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
        label: "Private HTTP focuser".into(),
        polling: PollPolicy {
            request_timeout_seconds: 1.0,
            poll_seconds: 1.0,
            ..PollPolicy::default()
        },
        backend: SourceBackend::Alpaca {
            base_url: format!("http://{address}/prefix/"),
            device_type: DeviceType::Focuser,
            device_number: 7,
            connection_policy: ConnectionPolicy::Managed,
            credential_reference: None,
        },
    };
    let source = SourceHandle::spawn(
        config.id,
        Uuid::new_v4(),
        config.polling.clone(),
        Box::new(
            AlpacaBackend::new(
                &config,
                regain_hub::focuser::FocuserProperty::ALL
                    .into_iter()
                    .map(|property| property.sample_request())
                    .collect(),
                None,
            )
            .unwrap(),
        ),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let controller = FocuserController::new(source.clone(), Duration::from_secs(5)).unwrap();
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    assert_eq!(first.generation(), second.generation());
    wait_samples(&source).await;
    assert!(source.snapshot().values["ismoving"].is_boolean());
    let error = first.step_size().await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert_eq!(error.upstream_code, Some(1024));
    assert!(!error.message.contains("private"));
    first.move_to(123).await.unwrap();
    assert!(second.is_moving().await.unwrap());
    second.halt().await.unwrap();
    assert_eq!(first.position().await.unwrap(), 123);
    second.set_temp_comp(false).await.unwrap();
    assert!(!first.temp_comp().await.unwrap());
    drop(first);
    settle().await;
    assert!(fixture.connected.load(SeqCst));
    drop(second);
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
    let writes = requests
        .iter()
        .filter(|(method, _, _)| method == "PUT")
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 5);
    assert_eq!(writes[0].1, "connected");
    assert_eq!(writes[4].1, "connected");
    assert_eq!(writes[1].1, "move");
    assert_eq!(writes[1].2["Position"], "123");
    let client = requests[0].2["ClientID"].clone();
    assert!(
        requests
            .iter()
            .all(|(_, _, args)| args["ClientID"] == client)
    );
    let transactions = requests
        .iter()
        .map(|(_, _, args)| args["ClientTransactionID"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(transactions.len(), requests.len());
    source.shutdown().await.unwrap();
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

fn runtime_setup(
    device: &Arc<Device>,
) -> (
    regain_hub::config::HubConfig,
    Arc<regain_hub::runtime::HubRuntime>,
    Arc<SourceHandle>,
) {
    use regain_hub::{
        config::{
            DeviceType, HubConfig, NativeDevice, OutputConfig, SourceBackend, SourceConfig,
            VirtualDevice,
        },
        runtime::HubRuntime,
        source::SourceRegistry,
    };
    let mut config = HubConfig::empty();
    let source_id = Uuid::new_v4();
    config.sources.push(SourceConfig {
        id: source_id,
        label: "Injected focuser".into(),
        polling: PollPolicy {
            request_timeout_seconds: 0.1,
            poll_seconds: 1.0,
            ..PollPolicy::default()
        },
        backend: SourceBackend::Native {
            camera: None,
            device: NativeDevice::Fc3,
            identity: "PRIVATE-TEST".into(),
            filter_wheel: None,
        },
    });
    for number in [4, 7] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Focuser {number}"),
            device: VirtualDevice::Proxy {
                source: source_id,
                device_type: DeviceType::Focuser,
            },
        });
    }
    let clock = Arc::new(MonotonicClock::default());
    let registry = Arc::new(
        SourceRegistry::build(&config, clock.clone(), |_| {
            Ok(Box::new(Mock(device.clone())))
        })
        .unwrap(),
    );
    let source = registry.get(source_id).unwrap();
    let runtime = HubRuntime::from_registry(config.clone(), registry, clock).unwrap();
    (config, runtime, source)
}
async fn wait_samples(source: &SourceHandle) {
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(3), async {
        while {
            let state = status.borrow_and_update();
            !state.values.contains_key("position") || !state.values.contains_key("ismoving")
        } {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn runtime_diagnostics_are_inert_paged_and_preserve_typed_errors_and_sample_ages() {
    use regain_hub::diagnostics::{Diagnostics, Reading};
    let device = Device::new();
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let inactive = runtime.output_status(output, 0, 4).unwrap();
    assert_eq!(inactive.total, 9);
    assert_eq!(inactive.next_start, Some(4));
    assert_eq!(device.connects.load(SeqCst), 0);
    assert_eq!(device.reads.load(SeqCst), 0);
    assert_eq!(device.polls.load(SeqCst), 0);
    assert_eq!(runtime.active_connections(), 0);
    assert_eq!(source.snapshot().lease_count, 0);
    let client = runtime.client();
    client.connect(output).await.unwrap();
    wait_samples(&source).await;
    let before_reads = device.reads.load(SeqCst);
    let before_polls = device.polls.load(SeqCst);
    let first = runtime.output_status(output, 0, 4).unwrap();
    let Diagnostics::Focuser { health, properties } = first.diagnostics else {
        panic!("Expected focuser diagnostics")
    };
    assert_eq!(
        health.generation,
        client
            .connection(output)
            .unwrap()
            .focuser()
            .unwrap()
            .generation()
    );
    assert_eq!(properties.len(), 4);
    assert!(
        properties
            .iter()
            .all(|item| matches!(item.sample, Reading::Available { .. }))
    );
    tokio::time::advance(Duration::from_millis(100)).await;
    let second = runtime.output_status(output, 4, 32).unwrap();
    assert_eq!(second.next_start, None);
    let Diagnostics::Focuser { properties, .. } = second.diagnostics else {
        panic!()
    };
    let Reading::Available { reading } = &properties[0].sample else {
        panic!()
    };
    assert!(reading.age_seconds >= 0.1);
    assert_eq!(device.reads.load(SeqCst), before_reads);
    assert_eq!(device.polls.load(SeqCst), before_polls);
    assert_eq!(source.snapshot().lease_count, 1);
    assert!(runtime.output_status(output, 10, 1).is_err());
    device.set("ismoving", json!(1));
    source.refresh(Uuid::nil()).await.unwrap_err();
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    let Diagnostics::Focuser { properties, .. } =
        runtime.output_status(output, 5, 1).unwrap().diagnostics
    else {
        panic!()
    };
    assert!(matches!(properties[0].sample, Reading::Unavailable { .. }));
    client.close();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn runtime_pending_connect_cancellation_and_generation_replacement_keep_admission_honest() {
    let device = Device::new();
    device.pending.store(true, SeqCst);
    let (config, runtime, source) = runtime_setup(&device);
    let client = runtime.client();
    let output = config.outputs[0].id;
    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.connect(output).await }
    });
    settle().await;
    assert_eq!(runtime.active_connections(), 1);
    assert!(client.connecting(output).unwrap());
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    settle().await;
    assert_eq!(runtime.active_connections(), 0);
    assert_eq!(source.snapshot().lease_count, 0);
    device.pending.store(false, SeqCst);
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    let old_generation = connection.focuser().unwrap().generation();
    *device.hang_read.lock().unwrap() = Some("maxincrement".into());
    assert_eq!(
        connection
            .focuser()
            .unwrap()
            .move_to(100)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Transient
    );
    assert!(!connection.focuser().unwrap().connected());
    assert_ne!(source.snapshot().generation, old_generation);
    *device.hang_read.lock().unwrap() = None;
    drop(connection);
    client.disconnect(output);
    settle().await;
    client.connect(output).await.unwrap();
    client
        .connection(output)
        .unwrap()
        .focuser()
        .unwrap()
        .move_to(100)
        .await
        .unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    client.close();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn actual_ipc_focuser_dispatch_preserves_class_ownership_eof_and_uncertainty() {
    use regain_hub::ipc::{Limits, read_frame, serve_stream};
    use tokio::io::{AsyncWriteExt, DuplexStream};
    async fn call(stream: &mut DuplexStream, id: u64, command: Value) -> Value {
        let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(&bytes).await.unwrap();
        serde_json::from_slice(
            &read_frame(stream, Duration::from_secs(2))
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
    let device = Device::new();
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let sibling = runtime.client();
    sibling.connect(config.outputs[1].id).await.unwrap();
    let (mut stream, server) = tokio::io::duplex(1024 * 1024);
    let server = tokio::spawn(serve_stream(server, runtime.clone(), Limits::default()));
    assert!(
        call(&mut stream, 1, json!({"op":"hello"}))
            .await
            .get("result")
            .is_some()
    );
    assert!(
        call(&mut stream, 2, json!({"op":"connect","output":output})).await["result"].is_null()
    );
    wait_samples(&source).await;
    assert_eq!(runtime.active_connections(), 2);
    let get = |property| json!({"op":"get","output":output,"property":property});
    assert_eq!(
        call(
            &mut stream,
            3,
            get(json!({"member":"focuser","property":"position"}))
        )
        .await["result"],
        50
    );
    assert_eq!(
        call(&mut stream, 4, get(json!({"member":"isSafe"}))).await["error"]["code"],
        "unsupported"
    );
    let before_reads = device.reads.load(SeqCst);
    let state = call(&mut stream, 5, get(json!({"member":"deviceState"}))).await;
    assert_eq!(state["result"].as_array().unwrap().len(), 3);
    assert_eq!(device.reads.load(SeqCst), before_reads);
    let put = |property| json!({"op":"put","output":output,"property":property});
    assert_eq!(
        call(
            &mut stream,
            6,
            put(json!({"member":"moveFocuser","position":1001}))
        )
        .await["error"]["code"],
        "invalidValue"
    );
    assert!(device.writes.lock().unwrap().is_empty());
    assert!(
        call(
            &mut stream,
            7,
            put(json!({"member":"moveFocuser","position":123}))
        )
        .await
        .get("result")
        .is_some()
    );
    assert!(
        sibling
            .connection(config.outputs[1].id)
            .unwrap()
            .focuser()
            .unwrap()
            .is_moving()
            .await
            .unwrap()
    );
    assert!(
        call(&mut stream, 8, put(json!({"member":"haltFocuser"})))
            .await
            .get("result")
            .is_some()
    );
    device.uncertain.store(true, SeqCst);
    assert_eq!(
        call(
            &mut stream,
            9,
            put(json!({"member":"moveFocuser","position":200}))
        )
        .await["error"]["code"],
        "uncertain"
    );
    assert_eq!(
        call(&mut stream, 10, get(json!({"member":"connected"}))).await["result"],
        false
    );
    assert_eq!(
        call(&mut stream, 11, put(json!({"member":"haltFocuser"}))).await["error"]["code"],
        "uncertain"
    );
    drop(stream);
    assert!(server.await.unwrap().is_ok());
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(runtime.active_connections(), 1);
    assert!(source.snapshot().write_uncertain);
    assert_eq!(device.writes.lock().unwrap().len(), 3);
    sibling.close();
    runtime.shutdown().await.unwrap();
}

#[test]
fn typed_ipc_rejects_extra_keys_wrong_types_and_out_of_range_positions() {
    for property in [
        json!({"member":"focuser","property":"position","extra":1}),
        json!({"member":"focuser","property":"commandBlind"}),
    ] {
        assert!(serde_json::from_value::<regain_hub::ipc::Get>(property).is_err());
    }
    for property in [
        json!({"member":"moveFocuser","position":2147483648_u64}),
        json!({"member":"moveFocuser","position":1.5}),
        json!({"member":"haltFocuser","position":1}),
        json!({"member":"focuserTempComp","enabled":"true"}),
    ] {
        assert!(serde_json::from_value::<regain_hub::ipc::Put>(property).is_err());
    }
}

#[test]
fn focuser_poll_plans_deduplicate_properties_across_multiple_outputs_without_io() {
    let device = Device::new();
    // Registry construction requires a runtime, so use a local executor solely
    // for this preparation check; no source lease or worker is acquired.
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        let (config, runtime, _) = runtime_setup(&device);
        let before = config.clone();
        let plans = regain_hub::factory::source_plans(&config).unwrap();
        let samples = &plans[&config.sources[0].id].samples;
        assert_eq!(samples.len(), 9);
        assert_eq!(config, before);
        for sample in samples {
            assert!(sample.parameters.is_empty());
            assert!(sample.sensor_age.is_none());
            let is_boolean = ["absolute", "ismoving", "tempcomp", "tempcompavailable"]
                .contains(&sample.member.as_str());
            assert_eq!(
                matches!(sample.value_type, regain_hub::sampling::SampleType::Boolean),
                is_boolean
            );
        }
        assert_eq!(device.connects.load(SeqCst), 0);
        runtime.shutdown().await.unwrap();
    });
}

// Coordination fixtures reuse the same actor, fences and typed session as all
// ordinary focuser clients. No native transport or installed driver is loaded.
async fn group_setup(
    count: usize,
) -> (
    Vec<Arc<Device>>,
    Vec<Arc<SourceHandle>>,
    Vec<Arc<regain_hub::focuser::FocuserSession>>,
) {
    let mut devices = Vec::new();
    let mut sources = Vec::new();
    let mut sessions = Vec::new();
    for _ in 0..count {
        let device = Device::new();
        device.set("tempcomp", json!(false));
        let (source, controller) = setup(&device);
        sessions.push(Arc::new(controller.connect().await.unwrap()));
        devices.push(device);
        sources.push(source);
    }
    (devices, sources, sessions)
}
fn group_config(sessions: &[Arc<regain_hub::focuser::FocuserSession>]) -> FocuserGroupConfig {
    FocuserGroupConfig {
        id: Uuid::new_v4(),
        label: "Private calibrated focusers".into(),
        minimum: 0,
        maximum: 1000,
        timeout_seconds: 2.0,
        poll_seconds: 0.01,
        members: sessions
            .iter()
            .map(|session| FocuserCalibration {
                source: session.source_id(),
                scale_numerator: 1,
                scale_denominator: 1,
                offset: 0,
                minimum: 0,
                maximum: 1000,
            })
            .collect(),
    }
}
async fn wrote(device: &Device) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let notified = device.written.notified();
            if !device.writes.lock().unwrap().is_empty() {
                return;
            }
            notified.await;
        }
    })
    .await
    .expect("Group did not dispatch the expected private move");
}
async fn shutdown_group(sources: &[Arc<SourceHandle>]) {
    for source in sources {
        source.shutdown().await.unwrap();
    }
}

#[test]
fn group_calibration_rounding_overflow_and_configuration_are_explicit() {
    let mut calibration = FocuserCalibration {
        source: Uuid::new_v4(),
        scale_numerator: 1,
        scale_denominator: 2,
        offset: 100,
        minimum: 0,
        maximum: 1000,
    };
    assert_eq!(calibration.target(1).unwrap(), 101);
    assert_eq!(calibration.target(-1).unwrap(), 99);
    assert_eq!(calibration.target(2).unwrap(), 101);
    calibration.scale_numerator = -1;
    assert_eq!(calibration.target(1).unwrap(), 99);
    calibration.scale_numerator = i32::MAX;
    assert_eq!(
        calibration.target(i32::MAX).unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    calibration.scale_denominator = 0;
    assert!(calibration.target(0).is_err());
    calibration.scale_denominator = 1;
    calibration.scale_numerator = 0;
    assert!(calibration.target(0).is_err());
}

#[tokio::test(start_paused = true)]
async fn group_calibrates_each_target_and_reserves_members_until_exact_completion() {
    let (devices, sources, sessions) = group_setup(2).await;
    let sibling = FocuserController::new(sources[0].clone(), Duration::from_secs(2))
        .unwrap()
        .connect()
        .await
        .unwrap();
    let mut config = group_config(&sessions);
    config.members[0].scale_numerator = 3;
    config.members[0].scale_denominator = 2;
    config.members[0].offset = 10;
    config.members[1].scale_numerator = -2;
    config.members[1].offset = 500;
    let group = FocuserGroup::new(config, sessions).unwrap();
    let mut operation = group.start(100).unwrap();
    assert_eq!(group.start(100).err().unwrap().kind, ErrorKind::Busy);
    for device in &devices {
        wrote(device).await;
    }
    assert_eq!(devices[0].writes.lock().unwrap()[0].1["Position"], 160);
    assert_eq!(devices[1].writes.lock().unwrap()[0].1["Position"], 300);
    assert_eq!(
        sibling.move_to(150).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        sibling.set_temp_comp(true).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(sibling.halt().await.unwrap_err().kind, ErrorKind::Busy);
    let before = devices[0].reads.load(SeqCst);
    let sequence = operation.status().sequence;
    for _ in 0..10 {
        assert_eq!(operation.status().sequence, sequence);
    }
    assert_eq!(devices[0].reads.load(SeqCst), before);
    for device in &devices {
        device.set("ismoving", json!(false));
    }
    let result = operation.completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::Complete);
    assert!(
        result
            .members
            .iter()
            .all(|member| member.phase == FocuserMemberPhase::Complete
                && member.last_position == Some(member.target))
    );
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().len() == 1)
    );
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_preflights_every_member_before_any_write() {
    for fault in [
        "travel",
        "increment",
        "compensation",
        "relative",
        "moving",
        "read",
    ] {
        let (devices, sources, sessions) = group_setup(2).await;
        match fault {
            "travel" => devices[1].set("maxstep", json!(90)),
            "increment" => devices[1].set("maxincrement", json!(20)),
            "compensation" => devices[1].set("tempcomp", json!(true)),
            "relative" => devices[1].set("absolute", json!(false)),
            "moving" => devices[1].set("ismoving", json!(true)),
            "read" => {
                devices[1]
                    .errors
                    .lock()
                    .unwrap()
                    .insert("maxstep".into(), SourceError::transient());
            }
            _ => unreachable!(),
        }
        let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
        let result = group.start(100).unwrap().completed().await.unwrap();
        assert_eq!(result.phase, FocuserGroupPhase::PreflightFailed, "{fault}");
        assert_eq!(
            result.members[1].phase,
            FocuserMemberPhase::Rejected,
            "{fault}"
        );
        assert!(
            devices
                .iter()
                .all(|device| device.writes.lock().unwrap().is_empty()),
            "{fault}"
        );
        shutdown_group(&sources).await;
    }
}

#[tokio::test(start_paused = true)]
async fn group_rejects_duplicate_mismatched_and_unbounded_configuration_without_io() {
    let (devices, sources, sessions) = group_setup(2).await;
    let baseline: Vec<_> = devices
        .iter()
        .map(|device| device.reads.load(SeqCst))
        .collect();
    let config = group_config(&sessions);
    for fault in [
        "duplicate",
        "mismatch",
        "empty",
        "nan",
        "long",
        "poll",
        "logical",
    ] {
        let mut invalid = config.clone();
        match fault {
            "duplicate" => invalid.members[1].source = invalid.members[0].source,
            "mismatch" => invalid.members[1].source = Uuid::new_v4(),
            "empty" => invalid.members.clear(),
            "nan" => invalid.timeout_seconds = f64::NAN,
            "long" => invalid.timeout_seconds = 301.0,
            "poll" => invalid.poll_seconds = 0.0,
            "logical" => invalid.maximum = -1,
            _ => unreachable!(),
        }
        assert!(
            FocuserGroup::new(invalid, sessions.clone()).is_err(),
            "{fault}"
        );
    }
    let group = FocuserGroup::new(config.clone(), sessions.clone()).unwrap();
    assert!(group.start(-1).is_err());
    assert!(group.start(1001).is_err());
    let mut outside = config;
    outside.members[1].maximum = 99;
    let other = FocuserGroup::new(outside, sessions).unwrap();
    assert!(other.start(100).is_err());
    assert_eq!(
        devices
            .iter()
            .map(|device| device.reads.load(SeqCst))
            .collect::<Vec<_>>(),
        baseline
    );
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().is_empty())
    );
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_partial_failure_observes_started_member_without_replay_or_rollback() {
    for uncertain in [false, true] {
        let (devices, sources, sessions) = group_setup(3).await;
        devices[1].uncertain.store(uncertain, SeqCst);
        if !uncertain {
            *devices[1].write_error.lock().unwrap() = Some(SourceError::new(
                ErrorKind::Unsupported,
                "Private rejected move",
            ));
        }
        let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
        let mut operation = group.start(100).unwrap();
        wrote(&devices[1]).await;
        devices[0].set("ismoving", json!(false));
        let result = operation.completed().await.unwrap();
        assert_eq!(result.phase, FocuserGroupPhase::PartialFailure);
        assert_eq!(result.members[0].phase, FocuserMemberPhase::Complete);
        assert_eq!(
            result.members[1].phase,
            if uncertain {
                FocuserMemberPhase::Uncertain
            } else {
                FocuserMemberPhase::Failed
            }
        );
        assert_eq!(result.members[2].phase, FocuserMemberPhase::NotStarted);
        assert_eq!(devices[0].writes.lock().unwrap().len(), 1);
        assert_eq!(devices[1].writes.lock().unwrap().len(), 1);
        assert!(devices[2].writes.lock().unwrap().is_empty());
        shutdown_group(&sources).await;
    }
}

#[tokio::test(start_paused = true)]
async fn group_cancellation_during_dispatched_move_preserves_ack_and_stops_remaining_dispatch() {
    let (devices, sources, sessions) = group_setup(2).await;
    let gate = Arc::new(tokio::sync::Notify::new());
    *devices[0].write_gate.lock().unwrap() = Some(gate.clone());
    let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
    let mut operation = group.start(100).unwrap();
    wrote(&devices[0]).await;
    operation.cancel();
    settle().await;
    assert!(
        !operation.status().phase.terminal(),
        "Cancellation discarded an in-flight mutation acknowledgement"
    );
    gate.notify_one();
    let result = operation.completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::Cancelled);
    assert_eq!(result.members[0].phase, FocuserMemberPhase::Moving);
    assert_eq!(result.members[1].phase, FocuserMemberPhase::NotStarted);
    assert_eq!(devices[0].writes.lock().unwrap().len(), 1);
    assert!(devices[1].writes.lock().unwrap().is_empty());
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_cancel_before_start_does_no_io_and_dropped_waiter_does_not_cancel() {
    let (devices, sources, sessions) = group_setup(2).await;
    let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
    let baseline: Vec<_> = devices
        .iter()
        .map(|device| device.reads.load(SeqCst))
        .collect();
    let mut cancelled = group.start(100).unwrap();
    cancelled.cancel();
    assert_eq!(
        cancelled.completed().await.unwrap().phase,
        FocuserGroupPhase::Cancelled
    );
    settle().await;
    assert_eq!(
        devices
            .iter()
            .map(|device| device.reads.load(SeqCst))
            .collect::<Vec<_>>(),
        baseline
    );
    let operation = group.start(100).unwrap();
    let mut observer = operation.clone();
    drop(operation);
    for device in &devices {
        wrote(device).await;
        device.set("ismoving", json!(false));
    }
    assert_eq!(
        observer.completed().await.unwrap().phase,
        FocuserGroupPhase::Complete
    );
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_deadline_does_not_claim_halt_or_reached_position() {
    let (devices, sources, sessions) = group_setup(2).await;
    let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
    let result = group.start(100).unwrap().completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::Deadline);
    assert!(
        result
            .members
            .iter()
            .all(|member| member.phase == FocuserMemberPhase::Moving)
    );
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().len() == 1)
    );
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_acknowledgement_without_motion_cannot_report_success() {
    let (devices, sources, sessions) = group_setup(2).await;
    devices[1].ignore_move.store(true, SeqCst);
    let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
    let mut operation = group.start(100).unwrap();
    wrote(&devices[1]).await;
    devices[0].set("ismoving", json!(false));
    let result = operation.completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, FocuserMemberPhase::Complete);
    assert_eq!(result.members[1].phase, FocuserMemberPhase::Failed);
    assert_eq!(result.members[1].last_position, Some(50));
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_rechecks_live_limits_before_later_dispatch_and_observes_prior_motion() {
    let (devices, sources, sessions) = group_setup(2).await;
    let gate = Arc::new(tokio::sync::Notify::new());
    *devices[0].write_gate.lock().unwrap() = Some(gate.clone());
    let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
    let mut operation = group.start(100).unwrap();
    wrote(&devices[0]).await;
    devices[1].set("maxstep", json!(90));
    gate.notify_one();
    loop {
        let report = operation.changed().await.unwrap();
        if report.members[1].phase == FocuserMemberPhase::Rejected {
            break;
        }
        assert!(!report.phase.terminal());
    }
    devices[0].set("ismoving", json!(false));
    let result = operation.completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, FocuserMemberPhase::Complete);
    assert_eq!(result.members[1].phase, FocuserMemberPhase::Rejected);
    assert!(devices[1].writes.lock().unwrap().is_empty());
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_observes_completed_sibling_while_another_member_read_is_hung() {
    let (devices, sources, sessions) = group_setup(2).await;
    let gate = Arc::new(tokio::sync::Notify::new());
    *devices[1].write_gate.lock().unwrap() = Some(gate.clone());
    let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
    let mut operation = group.start(100).unwrap();
    wrote(&devices[1]).await;
    *devices[0].hang_read.lock().unwrap() = Some("ismoving".into());
    gate.notify_one();
    loop {
        let report = operation.changed().await.unwrap();
        if report.members[1].phase == FocuserMemberPhase::Moving {
            break;
        }
    }
    devices[1].set("ismoving", json!(false));
    let start = tokio::time::Instant::now();
    loop {
        let report = operation.changed().await.unwrap();
        if report.members[1].phase == FocuserMemberPhase::Complete {
            assert_eq!(report.members[0].phase, FocuserMemberPhase::Moving);
            assert!(tokio::time::Instant::now().duration_since(start) < Duration::from_millis(100));
            break;
        }
    }
    let result = operation.completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, FocuserMemberPhase::Uncertain);
    assert_eq!(result.members[1].phase, FocuserMemberPhase::Complete);
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_ambiguous_write_deadline_blocks_replay_and_retains_member_uncertainty() {
    let (devices, sources, sessions) = group_setup(2).await;
    devices[0].hang_write.store(true, SeqCst);
    let group = FocuserGroup::new(group_config(&sessions), sessions.clone()).unwrap();
    let result = group.start(100).unwrap().completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, FocuserMemberPhase::Uncertain);
    assert_eq!(result.members[1].phase, FocuserMemberPhase::NotStarted);
    assert_eq!(devices[0].writes.lock().unwrap().len(), 1);
    assert!(devices[1].writes.lock().unwrap().is_empty());
    settle().await;
    let repeated = group.start(100).unwrap().completed().await.unwrap();
    assert_eq!(repeated.phase, FocuserGroupPhase::PreflightFailed);
    assert_eq!(devices[0].writes.lock().unwrap().len(), 1);
    assert!(sessions[0].move_to(100).await.is_err());
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_overlapping_groups_fail_busy_without_lock_order_deadlock() {
    let (devices, sources, sessions) = group_setup(2).await;
    let first = FocuserGroup::new(group_config(&sessions), sessions.clone()).unwrap();
    let reversed: Vec<_> = sessions.into_iter().rev().collect();
    let second = FocuserGroup::new(group_config(&reversed), reversed).unwrap();
    let mut a = first.start(100).unwrap();
    let mut b = second.start(100).unwrap();
    settle().await;
    for device in &devices {
        device.set("ismoving", json!(false));
    }
    let (a, b) = tokio::join!(a.completed(), b.completed());
    let outcomes = [a.unwrap(), b.unwrap()];
    assert!(
        outcomes
            .iter()
            .any(|report| report.phase == FocuserGroupPhase::PreflightFailed)
    );
    assert!(outcomes.iter().all(|report| matches!(
        report.phase,
        FocuserGroupPhase::Complete | FocuserGroupPhase::PreflightFailed
    )));
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().len() <= 1)
    );
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_deadline_starts_at_admission_before_the_owned_task_is_scheduled() {
    let (devices, sources, sessions) = group_setup(2).await;
    let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
    let mut operation = group.start(100).unwrap();
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(
        operation.completed().await.unwrap().phase,
        FocuserGroupPhase::Deadline
    );
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().is_empty())
    );
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_source_retirement_during_motion_cannot_publish_false_completion() {
    let (devices, sources, sessions) = group_setup(2).await;
    let gate = Arc::new(tokio::sync::Notify::new());
    *devices[1].write_gate.lock().unwrap() = Some(gate.clone());
    let group = FocuserGroup::new(group_config(&sessions), sessions).unwrap();
    let mut operation = group.start(100).unwrap();
    wrote(&devices[1]).await;
    sources[0].shutdown().await.unwrap();
    gate.notify_one();
    loop {
        let report = operation.changed().await.unwrap();
        if report.members[1].phase == FocuserMemberPhase::Moving {
            break;
        }
    }
    devices[1].set("ismoving", json!(false));
    let result = operation.completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, FocuserMemberPhase::Uncertain);
    assert_eq!(result.members[1].phase, FocuserMemberPhase::Complete);
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().len() == 1)
    );
    shutdown_group(&sources).await;
}

#[tokio::test(start_paused = true)]
async fn group_deadline_awaits_existing_mutation_bound_and_preserves_lost_acknowledgement() {
    let (devices, sources, sessions) = group_setup(2).await;
    devices[0].hang_write.store(true, SeqCst);
    let mut config = group_config(&sessions);
    config.timeout_seconds = 0.01;
    let group = FocuserGroup::new(config, sessions).unwrap();
    let start = tokio::time::Instant::now();
    let result = group.start(100).unwrap().completed().await.unwrap();
    assert_eq!(result.phase, FocuserGroupPhase::Deadline);
    assert_eq!(result.members[0].phase, FocuserMemberPhase::Uncertain);
    assert_eq!(result.members[1].phase, FocuserMemberPhase::NotStarted);
    assert!(tokio::time::Instant::now().duration_since(start) >= Duration::from_millis(100));
    assert_eq!(devices[0].writes.lock().unwrap().len(), 1);
    assert!(devices[1].writes.lock().unwrap().is_empty());
    shutdown_group(&sources).await;
}

fn hosted_config() -> regain_hub::config::HubConfig {
    let mut config: regain_hub::config::HubConfig =
        serde_json::from_str(include_str!("../examples/paired-focusers.json")).unwrap();
    for source in &mut config.sources {
        source.polling.request_timeout_seconds = 0.1;
    }
    let group = &mut config.focuser_groups[0];
    group.minimum = 0;
    group.maximum = 1000;
    group.timeout_seconds = 2.0;
    group.poll_seconds = 0.01;
    group.members[1].offset = 20;
    config
}
fn hosted_devices() -> Arc<Vec<Arc<Device>>> {
    Arc::new(
        (0..3)
            .map(|_| {
                let device = Device::new();
                device.set("tempcomp", json!(false));
                device
            })
            .collect(),
    )
}
fn hosted_runtime(
    config: regain_hub::config::HubConfig,
    devices: &Arc<Vec<Arc<Device>>>,
) -> Arc<regain_hub::runtime::HubRuntime> {
    let clock = Arc::new(MonotonicClock::default());
    let registry = regain_hub::source::SourceRegistry::build(&config, clock.clone(), |source| {
        let index = config
            .sources
            .iter()
            .position(|item| item.id == source.id)
            .unwrap();
        Ok(Box::new(Mock(devices[index].clone())))
    })
    .unwrap();
    regain_hub::runtime::HubRuntime::from_registry(config, Arc::new(registry), clock).unwrap()
}
async fn hosted_send(stream: &mut tokio::io::DuplexStream, id: u64, command: Value) {
    use tokio::io::AsyncWriteExt;
    let bytes = serde_json::to_vec(&json!({"version":1, "id":id, "command":command})).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .unwrap();
    stream.write_all(&bytes).await.unwrap();
}
async fn hosted_call(stream: &mut tokio::io::DuplexStream, id: u64, command: Value) -> Value {
    hosted_send(stream, id, command).await;
    serde_json::from_slice(
        &regain_hub::ipc::read_frame(stream, Duration::from_secs(2))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
async fn hosted_stream(
    service: Arc<regain_hub::service::HubService>,
) -> (
    tokio::io::DuplexStream,
    tokio::task::JoinHandle<Result<(), regain_hub::ipc::ProtocolError>>,
) {
    let (mut stream, server) = tokio::io::duplex(1024 * 1024);
    let server = tokio::spawn(regain_hub::ipc::serve_service_stream(
        server,
        service,
        regain_hub::ipc::Limits::default(),
    ));
    let hello = hosted_call(&mut stream, 1, json!({"op":"hello"})).await;
    assert!(
        hello["result"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("focuserGroups"))
    );
    assert!(
        hello["result"]["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("startFocuserGroup"))
    );
    (stream, server)
}
async fn hosted_terminal(
    runtime: &regain_hub::runtime::HubRuntime,
    revision: Uuid,
    group: Uuid,
) -> regain_hub::coordination::HostedFocuserStatus {
    use regain_hub::coordination::HostedFocuserPhase;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let status = runtime.focuser_group_status(revision, group, None).unwrap();
            if !matches!(
                status.phase,
                HostedFocuserPhase::Connecting | HostedFocuserPhase::Running
            ) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("Hosted group did not finish")
}
async fn hosted_idle(runtime: &regain_hub::runtime::HubRuntime) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while runtime.active_connections() != 0
            || runtime
                .source_snapshots()
                .iter()
                .any(|source| source.lease_count != 0)
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("Hosted group did not release retained activity/leases");
}

#[tokio::test(start_paused = true)]
async fn hosted_group_ipc_reattaches_after_lost_ack_without_replay_and_retires_old_inventory() {
    use regain_hub::coordination::HostedFocuserPhase;
    for read_ack in [false, true] {
        let config = hosted_config();
        let revision = config.revision;
        let group = config.focuser_groups[0].id;
        let devices = hosted_devices();
        let runtime = hosted_runtime(config.clone(), &devices);
        let service = regain_hub::service::HubService::read_only(runtime.clone());
        let (mut first, first_server) = hosted_stream(service.clone()).await;
        let query = |operation: Option<Uuid>| json!({"op":"focuserGroupStatus", "group":group, "operation":operation, "expectedRevision":revision});
        assert_eq!(
            hosted_call(&mut first, 2, query(None)).await["error"]["code"],
            "unavailable"
        );
        assert!(
            devices
                .iter()
                .all(|device| device.connects.load(SeqCst) == 0)
        );
        let start = json!({"op":"startFocuserGroup", "group":group, "target":100, "expectedRevision":revision});
        let acknowledged = if read_ack {
            Some(hosted_call(&mut first, 3, start.clone()).await["result"].clone())
        } else {
            hosted_send(&mut first, 3, start).await;
            None
        };
        wrote(&devices[0]).await;
        wrote(&devices[1]).await;
        assert!(runtime.active_connections() > 0);
        assert_eq!(
            runtime.source_snapshots()[2].lease_count,
            0,
            "Virtual alias must not open an additional transport"
        );
        drop(first);
        let _ = first_server.await.unwrap();
        let (mut second, second_server) = hosted_stream(service.clone()).await;
        let recovered = hosted_call(&mut second, 2, query(None)).await["result"].clone();
        let operation: Uuid = serde_json::from_value(recovered["operation"].clone()).unwrap();
        assert_eq!(recovered["configurationRevision"], json!(revision));
        assert_eq!(recovered["hostInstance"], json!(service.host_id()));
        assert_eq!(
            recovered["bindings"][0]["configuredSource"],
            json!(config.sources[2].id)
        );
        assert_eq!(
            recovered["bindings"][0]["physicalSource"],
            json!(config.sources[0].id)
        );
        if let Some(acknowledged) = acknowledged {
            assert_eq!(acknowledged["operation"], recovered["operation"]);
        }
        assert_eq!(hosted_call(&mut second, 3, json!({"op":"startFocuserGroup", "group":group, "target":100, "expectedRevision":revision})).await["error"]["code"], "busy");
        assert_eq!(hosted_call(&mut second, 4, json!({"op":"cancelFocuserGroup", "group":group, "operation":Uuid::new_v4(), "expectedRevision":revision})).await["error"]["code"], "invalidValue");
        for device in devices.iter().take(2) {
            device.set("ismoving", json!(false));
        }
        let complete = hosted_terminal(&runtime, revision, group).await;
        assert_eq!(complete.phase, HostedFocuserPhase::Complete);
        assert_eq!(complete.result.unwrap().members[1].target, 120);
        hosted_idle(&runtime).await;
        assert!(
            devices
                .iter()
                .take(2)
                .all(|device| device.writes.lock().unwrap().len() == 1)
        );
        assert!(devices[2].writes.lock().unwrap().is_empty());
        let next = hosted_call(&mut second, 5, json!({"op":"startFocuserGroup", "group":group, "target":120, "expectedRevision":revision})).await["result"].clone();
        let next_id: Uuid = serde_json::from_value(next["operation"].clone()).unwrap();
        assert_ne!(next_id, operation);
        assert_eq!(
            hosted_call(&mut second, 6, query(Some(operation))).await["error"]["code"],
            "unavailable"
        );
        assert_eq!(hosted_call(&mut second, 7, json!({"op":"cancelFocuserGroup", "group":group, "operation":next_id, "expectedRevision":revision})).await["result"]["operation"], json!(next_id));
        assert_eq!(
            hosted_terminal(&runtime, revision, group).await.phase,
            HostedFocuserPhase::Cancelled
        );
        drop(second);
        let _ = second_server.await.unwrap();
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn hosted_group_pending_connection_blocks_apply_after_eof_and_old_revision_cannot_replay() {
    use regain_hub::{
        config::ConfigStore,
        coordination::HostedFocuserPhase,
        service::{HubService, RuntimeBuilder, UpdateError},
    };
    let config = hosted_config();
    let revision = config.revision;
    let group = config.focuser_groups[0].id;
    let devices = hosted_devices();
    devices[0].pending.store(true, SeqCst);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hub.json");
    let store = ConfigStore::new(Some(path.clone()), config.clone()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&store.snapshot()).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let builder: Arc<RuntimeBuilder> = Arc::new({
        let devices = devices.clone();
        move |config| Ok(hosted_runtime(config, &devices))
    });
    let service = HubService::persistent(store, builder).unwrap();
    let runtime = service.runtime().unwrap();
    let (mut first, server) = hosted_stream(service.clone()).await;
    let ack = hosted_call(
        &mut first,
        2,
        json!({"op":"startFocuserGroup", "group":group, "target":100, "expectedRevision":revision}),
    )
    .await;
    let operation: Uuid = serde_json::from_value(ack["result"]["operation"].clone()).unwrap();
    drop(first);
    let _ = server.await.unwrap();
    assert!(runtime.active_connections() > 0);
    let mut candidate = service.configuration();
    candidate.focuser_groups[0].label = "New saved group revision".into();
    assert!(matches!(
        service.apply(revision, candidate.clone()).await,
        Err(UpdateError::Connected)
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let (mut second, server) = hosted_stream(service.clone()).await;
    assert_eq!(hosted_call(&mut second, 2, json!({"op":"cancelFocuserGroup", "group":group, "operation":operation, "expectedRevision":Uuid::new_v4()})).await["error"]["code"], "revisionConflict");
    hosted_call(&mut second, 3, json!({"op":"cancelFocuserGroup", "group":group, "operation":operation, "expectedRevision":revision})).await;
    assert_eq!(
        hosted_terminal(&runtime, revision, group).await.phase,
        HostedFocuserPhase::Cancelled
    );
    hosted_idle(&runtime).await;
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().is_empty())
    );
    assert_eq!(devices[1].connects.load(SeqCst), 0);
    service.apply(revision, candidate).await.unwrap();
    let current = service.configuration();
    assert_ne!(current.revision, revision);
    assert_eq!(hosted_call(&mut second, 4, json!({"op":"startFocuserGroup", "group":group, "target":100, "expectedRevision":revision})).await["error"]["code"], "revisionConflict");
    assert_eq!(hosted_call(&mut second, 5, json!({"op":"focuserGroupStatus", "group":group, "operation":operation, "expectedRevision":current.revision})).await["error"]["code"], "unavailable");
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().is_empty())
    );
    drop(second);
    let _ = server.await.unwrap();
    service.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn hosted_group_shutdown_preserves_inflight_ack_and_does_not_halt_or_dispatch_more() {
    use regain_hub::coordination::HostedFocuserPhase;
    let config = hosted_config();
    let revision = config.revision;
    let group = config.focuser_groups[0].id;
    let devices = hosted_devices();
    let gate = Arc::new(tokio::sync::Notify::new());
    *devices[0].write_gate.lock().unwrap() = Some(gate.clone());
    let runtime = hosted_runtime(config, &devices);
    let accepted = runtime
        .start_focuser_group(runtime.runtime_id(), revision, group, 100)
        .unwrap();
    wrote(&devices[0]).await;
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    settle().await;
    assert!(!shutdown.is_finished());
    assert!(
        runtime
            .start_focuser_group(runtime.runtime_id(), revision, group, 100)
            .is_err()
    );
    gate.notify_one();
    shutdown.await.unwrap().unwrap();
    let result = runtime
        .focuser_group_status(revision, group, Some(accepted.operation))
        .unwrap();
    assert_eq!(result.phase, HostedFocuserPhase::Cancelled);
    assert_eq!(
        result.result.unwrap().members[0].phase,
        FocuserMemberPhase::Moving
    );
    assert_eq!(devices[0].writes.lock().unwrap().len(), 1);
    assert!(devices[1].writes.lock().unwrap().is_empty());
    assert_eq!(runtime.active_connections(), 0);
    assert!(
        runtime
            .source_snapshots()
            .iter()
            .all(|source| !source.transport_connected && source.lease_count == 0)
    );
}

#[tokio::test(start_paused = true)]
async fn hosted_group_connection_failure_identifies_member_without_motion_or_retained_activity() {
    use regain_hub::coordination::HostedFocuserPhase;
    let config = hosted_config();
    let revision = config.revision;
    let group = config.focuser_groups[0].id;
    let devices = hosted_devices();
    devices[1].errors.lock().unwrap().insert(
        "maxstep".into(),
        SourceError::new(ErrorKind::Unsupported, "Private absent capability"),
    );
    let runtime = hosted_runtime(config.clone(), &devices);
    assert!(
        runtime
            .start_focuser_group(runtime.runtime_id(), Uuid::new_v4(), group, 100)
            .is_err()
    );
    assert_eq!(runtime.active_connections(), 0);
    runtime
        .start_focuser_group(runtime.runtime_id(), revision, group, 100)
        .unwrap();
    let failed = hosted_terminal(&runtime, revision, group).await;
    assert_eq!(failed.phase, HostedFocuserPhase::Failed);
    assert_eq!(failed.failed_source, Some(config.sources[1].id));
    assert_eq!(failed.error.unwrap().kind, ErrorKind::Unsupported);
    hosted_idle(&runtime).await;
    assert!(
        devices
            .iter()
            .all(|device| device.writes.lock().unwrap().is_empty())
    );
    runtime.shutdown().await.unwrap();
}

#[test]
fn hosted_group_ipc_rejects_unknown_fields_missing_revision_and_noninteger_targets() {
    let group = Uuid::new_v4();
    let revision = Uuid::new_v4();
    for command in [
        json!({"op":"startFocuserGroup", "group":group, "target":1}),
        json!({"op":"startFocuserGroup", "group":group, "target":1.5, "expectedRevision":revision}),
        json!({"op":"startFocuserGroup", "group":group, "target":2147483648_i64, "expectedRevision":revision}),
        json!({"op":"startFocuserGroup", "group":group, "target":1, "expectedRevision":revision, "retry":true}),
        json!({"op":"cancelFocuserGroup", "group":group, "expectedRevision":revision}),
    ] {
        assert!(serde_json::from_value::<regain_hub::ipc::Command>(command).is_err());
    }
}
