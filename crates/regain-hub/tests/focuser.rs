//! Typed proxy faults are private actors, never physical motion.
use regain_hub::{
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
    connects: AtomicUsize,
    disconnects: AtomicUsize,
    uncertain: AtomicBool,
    hang_write: AtomicBool,
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
            connects: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
            uncertain: AtomicBool::new(false),
            hang_write: AtomicBool::new(false),
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
            if self.0.hang_write.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.0.uncertain.load(SeqCst) {
                return Err(SourceError::uncertain());
            }
            match member.as_str() {
                "move" => {
                    self.0.set("position", parameters["Position"].clone());
                    self.0.set("ismoving", json!(true));
                }
                "halt" => self.0.set("ismoving", json!(false)),
                "tempcomp" => self.0.set("tempcomp", parameters["TempComp"].clone()),
                _ => panic!("Unexpected command {member}"),
            }
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async { Ok(Values::new()) })
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
        Box::new(AlpacaBackend::new(&config, Vec::new(), None).unwrap()),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let controller = FocuserController::new(source.clone(), Duration::from_secs(5)).unwrap();
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    assert_eq!(first.generation(), second.generation());
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
