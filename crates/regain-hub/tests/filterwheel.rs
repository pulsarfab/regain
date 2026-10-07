//! Private actors and loopback upstreams only; no physical wheel or calibration.
use regain_hub::{
    filterwheel::{FilterWheelController, FilterWheelProperty, MAX_FILTER_SLOTS},
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
    writes: Mutex<Vec<(String, Values)>>,
    reads: AtomicUsize,
    polls: AtomicUsize,
    errors: Mutex<std::collections::BTreeMap<String, SourceError>>,
    ages: Mutex<std::collections::BTreeMap<String, f64>>,
    connects: AtomicUsize,
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
                ("names".into(), json!(["L", "Hα", ""])),
                ("focusoffsets".into(), json!([-12, 0, 17])),
                ("position".into(), json!(0)),
            ])),
            writes: Mutex::default(),
            reads: AtomicUsize::new(0),
            polls: AtomicUsize::new(0),
            errors: Mutex::default(),
            ages: Mutex::default(),
            connects: AtomicUsize::new(0),
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
    fn command(&self, member: String, parameters: Values) {
        assert_eq!(member, "position");
        assert_eq!(parameters.len(), 1);
        assert!(parameters["Position"].is_i64());
        self.writes.lock().unwrap().push((member, parameters));
        self.set("position", json!(-1));
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
            Ok(self.0.values.lock().unwrap()[&member].clone())
        })
    }
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.0.command(member, parameters);
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
            let mut values = self.poll().await?;
            let errors = self.0.errors.lock().unwrap().clone();
            let mut ages_seconds = self.0.ages.lock().unwrap().clone();
            for key in errors.keys() {
                values.remove(key);
                ages_seconds.remove(key);
            }
            Ok(regain_hub::source::SampleBatch {
                values,
                errors,
                ages_seconds,
                ..Default::default()
            })
        })
    }
    fn reset(&mut self) {}
}
fn setup(device: &Arc<Device>) -> (Arc<SourceHandle>, FilterWheelController) {
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
    let controller = FilterWheelController::new(source.clone(), Duration::from_secs(2)).unwrap();
    (source, controller)
}
async fn settle() {
    for _ in 0..24 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn independent_leases_share_ordered_metadata_and_actual_motion_without_implicit_commands() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    assert_eq!(device.connects.load(SeqCst), 0);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    assert_eq!(device.connects.load(SeqCst), 1);
    assert_eq!(source.snapshot().lease_count, 2);
    assert_eq!(first.generation(), second.generation());
    assert_eq!(
        first.property(FilterWheelProperty::Names).await.unwrap(),
        json!(["L", "Hα", ""])
    );
    assert_eq!(
        second
            .property(FilterWheelProperty::FocusOffsets)
            .await
            .unwrap(),
        json!([-12, 0, 17])
    );
    first.move_to(2).await.unwrap();
    assert_eq!(second.position().await.unwrap(), -1);
    assert_eq!(second.move_to(1).await.unwrap_err().kind, ErrorKind::Busy);
    drop(first);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.disconnects.load(SeqCst), 0);
    assert_eq!(second.position().await.unwrap(), -1);
    device.set("position", json!(2));
    assert_eq!(second.position().await.unwrap(), 2);
    drop(second);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    assert_eq!(device.disconnects.load(SeqCst), 1);
    assert_eq!(
        *device.writes.lock().unwrap(),
        vec![(
            "position".into(),
            Values::from([("Position".into(), json!(2))])
        )]
    );
    source.shutdown().await.unwrap();
}

#[test]
fn wire_properties_reject_wrong_types_overflow_and_resource_exhaustion_without_coercion() {
    use FilterWheelProperty::*;
    for (property, value) in [
        (Position, json!(-2)),
        (Position, json!(1.5)),
        (Position, json!("0")),
        (Position, json!(i32::MAX)),
        (Names, json!([])),
        (Names, json!([1])),
        (Names, json!("L")),
        (Names, json!(vec!["L"; MAX_FILTER_SLOTS + 1])),
        (Names, json!(["λ".repeat(524_289)])),
        (FocusOffsets, json!([])),
        (FocusOffsets, json!([0, 1.5])),
        (FocusOffsets, json!([0, "1"])),
        (FocusOffsets, json!([0, i64::MAX])),
        (FocusOffsets, json!([3, 4])),
        (FocusOffsets, json!(vec![0; MAX_FILTER_SLOTS + 1])),
    ] {
        assert_eq!(
            property.decode(&value).unwrap_err().kind,
            ErrorKind::Unavailable
        );
    }
    for position in [-1, 0, MAX_FILTER_SLOTS as i32 - 1] {
        Position.decode(&json!(position)).unwrap();
    }
    Names.decode(&json!(vec!["L"; MAX_FILTER_SLOTS])).unwrap();
    FocusOffsets
        .decode(&json!([i32::MIN, 0, i32::MAX]))
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn every_move_checks_live_slot_count_pairing_and_stationary_position() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    let reads = device.reads.load(SeqCst);
    for position in [-1, i32::MIN, i32::MAX, MAX_FILTER_SLOTS as i32] {
        assert_eq!(
            session.move_to(position).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(device.reads.load(SeqCst), reads);
    assert_eq!(
        session.move_to(3).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    device.set("names", json!(["Reference"]));
    assert_eq!(
        session.capabilities().await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        session.move_to(0).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    device.set("focusoffsets", json!([0]));
    assert_eq!(
        session.move_to(1).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    for value in [json!(-2), json!(1), json!(1.5), json!("0")] {
        device.set("position", value);
        assert_eq!(
            session.position().await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            session.move_to(0).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
    }
    assert!(device.writes.lock().unwrap().is_empty());
    device.set("position", json!(0));
    session.move_to(0).await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn invalid_or_pending_initial_readiness_releases_only_its_own_connection_lease() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    assert!(FilterWheelController::new(source.clone(), Duration::ZERO).is_err());
    assert!(FilterWheelController::new(source.clone(), Duration::from_secs(301)).is_err());
    let first = controller.connect().await.unwrap();
    device.set("focusoffsets", json!([0]));
    assert!(
        matches!(controller.connect().await, Err(error) if error.kind == ErrorKind::Unavailable)
    );
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    drop(first);
    settle().await;
    device.set("focusoffsets", json!([-12, 0, 17]));
    device.set("position", json!(-1));
    let moving = controller.connect().await.unwrap();
    drop(moving);
    settle().await;
    device.pending.store(true, SeqCst);
    assert!(
        matches!(controller.connect().await, Err(error) if error.kind == ErrorKind::Unavailable)
    );
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    assert!(device.writes.lock().unwrap().is_empty());
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn uncertain_applied_position_fences_all_clients_and_never_replays_a_move() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    device.uncertain.store(true, SeqCst);
    assert_eq!(
        first.move_to(2).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    device.uncertain.store(false, SeqCst);
    assert!(!first.connected());
    assert!(!second.connected());
    assert_eq!(
        second.move_to(1).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert!(source.snapshot().write_uncertain);
    assert_eq!(device.values.lock().unwrap()["position"], -1);
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    drop(first);
    settle().await;
    assert!(source.snapshot().write_uncertain);
    drop(second);
    settle().await;
    device.set("position", json!(2));
    let fresh = controller.connect().await.unwrap();
    assert_eq!(fresh.position().await.unwrap(), 2);
    fresh.move_to(1).await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn preflight_transport_loss_prevents_dispatch_and_old_sessions_cannot_adopt_reconnection() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let first = controller.connect().await.unwrap();
    let generation = first.generation();
    *device.hang_read.lock().unwrap() = Some("focusoffsets".into());
    assert_eq!(
        first.move_to(1).await.unwrap_err().kind,
        ErrorKind::Transient
    );
    assert!(device.writes.lock().unwrap().is_empty());
    assert_ne!(source.snapshot().generation, generation);
    *device.hang_read.lock().unwrap() = None;
    let fresh = controller.connect().await.unwrap_or_else(|error| {
        panic!(
            "fresh connection failed: {error}; source={}",
            serde_json::to_string(&source.snapshot()).unwrap()
        )
    });
    assert_ne!(fresh.generation(), generation);
    assert_eq!(
        first.move_to(1).await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    fresh.move_to(1).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_connection_and_preflight_release_their_leases_without_dispatch_or_uncertainty() {
    let device = Device::new();
    device.pending.store(true, SeqCst);
    let (source, controller) = setup(&device);
    let controller = Arc::new(controller);
    let connect = tokio::spawn({
        let controller = controller.clone();
        async move { controller.connect().await }
    });
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.reads.load(SeqCst), 0);
    connect.abort();
    assert!(matches!(connect.await, Err(error) if error.is_cancelled()));
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    device.pending.store(false, SeqCst);
    let session = Arc::new(controller.connect().await.unwrap());
    *device.hang_read.lock().unwrap() = Some("names".into());
    let command = tokio::spawn({
        let session = session.clone();
        async move { session.move_to(1).await }
    });
    settle().await;
    command.abort();
    assert!(command.await.unwrap_err().is_cancelled());
    *device.hang_read.lock().unwrap() = None;
    tokio::time::advance(Duration::from_millis(100)).await;
    settle().await;
    assert!(device.writes.lock().unwrap().is_empty());
    assert!(!source.snapshot().write_uncertain);
    assert!(!session.connected());
    assert_eq!(source.snapshot().lease_count, 1);
    drop(session);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn concurrent_moves_share_control_and_cancelling_dispatch_retains_uncertainty() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = Arc::new(controller.connect().await.unwrap());
    let (a, b) = tokio::join!(session.move_to(1), session.move_to(2));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(a.err().or(b.err()).unwrap().kind, ErrorKind::Busy);
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    device.set("position", json!(1));
    device.hang_write.store(true, SeqCst);
    let command = tokio::spawn({
        let session = session.clone();
        async move { session.move_to(2).await }
    });
    settle().await;
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    command.abort();
    assert!(command.await.unwrap_err().is_cancelled());
    tokio::time::advance(Duration::from_millis(100)).await;
    settle().await;
    assert!(source.snapshot().write_uncertain);
    assert_eq!(
        session.move_to(0).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    source.shutdown().await.unwrap();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WheelTransportCase {
    Direct,
    Nested,
    CancelNested,
    LoseNestedGeneration,
    Cache,
}
async fn alpaca_wheel(version: u16, lose_reply: bool, case: WheelTransportCase) {
    use axum::{
        Json, Router,
        body::to_bytes,
        extract::{Request, State},
        routing::any,
    };
    use regain_hub::{
        alpaca::AlpacaBackend,
        config::{ConnectionPolicy, DeviceType, SourceBackend, SourceConfig},
        source::ConnectionMethod,
    };
    #[derive(Clone)]
    struct Fixture {
        device: Arc<Device>,
        connected: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<(String, String, Values)>>>,
        version: u16,
        lose_reply: bool,
    }
    async fn handler(State(state): State<Fixture>, request: Request) -> Json<Value> {
        assert!(
            request
                .uri()
                .path()
                .starts_with("/private/api/v1/filterwheel/7/")
        );
        let member = request.uri().path().rsplit('/').next().unwrap().to_owned();
        let method = request.method().as_str().to_owned();
        let data = if method == "GET" {
            request.uri().query().unwrap().as_bytes().to_vec()
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
                    assert_eq!(state.version, 2);
                    state
                        .connected
                        .store(parameters["Connected"] == "true", SeqCst);
                }
                "connect" | "disconnect" => {
                    assert_eq!(state.version, 3);
                    assert_eq!(parameters.len(), 2);
                    state.connected.store(member == "connect", SeqCst);
                }
                "position" => {
                    assert!(state.connected.load(SeqCst));
                    state.device.command(
                        member,
                        Values::from([(
                            "Position".into(),
                            json!(
                                parameters["Position"]
                                    .as_str()
                                    .unwrap()
                                    .parse::<i32>()
                                    .unwrap()
                            ),
                        )]),
                    );
                    if state.lose_reply {
                        return Json(json!({"ErrorNumber":"malformed acknowledgment"}));
                    }
                }
                _ => panic!("Unexpected wheel command {member}"),
            }
            Value::Null
        } else {
            match member.as_str() {
                "interfaceversion" => {
                    if state.device.pending.load(SeqCst) {
                        tokio::time::sleep(Duration::from_millis(700)).await;
                    }
                    json!(state.version)
                }
                "connected" => json!(state.connected.load(SeqCst)),
                "connecting" => {
                    assert_eq!(state.version, 3);
                    json!(false)
                }
                _ => {
                    let hang = state.device.hang_read.lock().unwrap().as_ref() == Some(&member);
                    if hang {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    state.device.values.lock().unwrap()[&member].clone()
                }
            }
        };
        Json(
            json!({"ErrorNumber":0,"Value":value,"ClientTransactionID":parameters["ClientTransactionID"].as_str().unwrap().parse::<u32>().unwrap()}),
        )
    }
    let fixture = Fixture {
        device: Device::new(),
        connected: Arc::new(AtomicBool::new(false)),
        requests: Arc::new(Mutex::new(Vec::new())),
        version,
        lose_reply,
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
        label: "Private wheel".into(),
        polling: PollPolicy {
            request_timeout_seconds: 1.0,
            ..PollPolicy::default()
        },
        backend: SourceBackend::Alpaca {
            unique_id: None,
            base_url: format!("http://{address}/private/"),
            device_type: DeviceType::FilterWheel,
            device_number: 7,
            connection_policy: ConnectionPolicy::Managed,
            credential_reference: None,
        },
    };
    if case != WheelTransportCase::Direct {
        virtual_wheel::nested(
            config,
            fixture.device.clone(),
            fixture.connected.clone(),
            fixture.requests.clone(),
            version,
            lose_reply,
            case,
        )
        .await;
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        return;
    }
    let backend = AlpacaBackend::new(&config, vec![], None).unwrap();
    let source = SourceHandle::spawn(
        config.id,
        Uuid::new_v4(),
        config.polling,
        Box::new(backend),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let controller = FilterWheelController::new(source.clone(), Duration::from_secs(3)).unwrap();
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    let connection = source.snapshot().connection_info.unwrap();
    assert_eq!(connection.interface_version, Some(version));
    assert_eq!(
        connection.method,
        if version == 3 {
            ConnectionMethod::Async
        } else {
            ConnectionMethod::Legacy
        }
    );
    assert_eq!(
        first.property(FilterWheelProperty::Names).await.unwrap(),
        json!(["L", "Hα", ""])
    );
    assert_eq!(
        second
            .property(FilterWheelProperty::FocusOffsets)
            .await
            .unwrap(),
        json!([-12, 0, 17])
    );
    if lose_reply {
        assert_eq!(
            first.move_to(2).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            second.move_to(1).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert!(!first.connected());
        assert!(!second.connected());
        assert!(source.snapshot().write_uncertain);
    } else {
        first.move_to(2).await.unwrap();
        assert_eq!(second.position().await.unwrap(), -1);
        fixture.device.set("position", json!(2));
        assert_eq!(second.position().await.unwrap(), 2);
    }
    assert_eq!(fixture.device.writes.lock().unwrap().len(), 1);
    drop(first);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    drop(second);
    source.shutdown().await.unwrap();
    assert!(!fixture.connected.load(SeqCst));
    let requests = fixture.requests.lock().unwrap().clone();
    let mut transactions = std::collections::BTreeSet::new();
    for (_, _, parameters) in &requests {
        assert!(
            transactions.insert(
                parameters["ClientTransactionID"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            )
        );
    }
    assert_eq!(
        requests
            .iter()
            .filter(|(method, member, _)| method == "PUT" && member == "position")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|(method, member, _)| method == "PUT"
                && (member == "connect" || member == "connected"))
            .count(),
        if version == 3 { 1 } else { 2 }
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
#[tokio::test]
async fn actual_alpaca_v2_wheel_preserves_arrays_position_and_legacy_ownership() {
    alpaca_wheel(2, false, WheelTransportCase::Direct).await;
}
#[tokio::test]
async fn actual_alpaca_v3_wheel_preserves_arrays_position_and_async_ownership() {
    alpaca_wheel(3, false, WheelTransportCase::Direct).await;
}
#[tokio::test]
async fn actual_alpaca_wheel_lost_reply_never_replays_the_applied_position() {
    alpaca_wheel(3, true, WheelTransportCase::Direct).await;
}

#[path = "support/filterwheel_runtime.rs"]
mod runtime;

#[path = "support/filterwheel_virtual.rs"]
mod virtual_wheel;
