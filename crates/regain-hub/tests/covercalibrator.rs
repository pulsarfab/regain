//! Private panels and loopback HTTP only; never installed drivers or hardware.
use regain_hub::{
    config::DeviceType,
    covercalibrator::{CoverCalibratorController, CoverCalibratorProperty as Property},
    parameters::PollPolicy,
    safety::MonotonicClock,
    source::{
        Backend, BackendFuture, ConnectionInfo, ConnectionMethod, ErrorKind, SourceError,
        SourceHandle, Values,
    },
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
    reads: Mutex<Vec<String>>,
    writes: Mutex<Vec<(String, Values)>>,
    version: AtomicUsize,
    connects: AtomicUsize,
    disconnects: AtomicUsize,
    pending: AtomicBool,
    uncertain: AtomicBool,
    hang_write: AtomicBool,
    hang_read: Mutex<Option<String>>,
    errors: Mutex<std::collections::BTreeMap<String, SourceError>>,
    ages: Mutex<std::collections::BTreeMap<String, f64>>,
    polls: AtomicUsize,
}
impl Device {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            values: Mutex::new(Values::from([
                ("coverstate".into(), json!(1)),
                ("calibratorstate".into(), json!(1)),
                ("brightness".into(), json!(0)),
                ("maxbrightness".into(), json!(4096)),
                ("covermoving".into(), json!(false)),
                ("calibratorchanging".into(), json!(false)),
            ])),
            reads: Mutex::default(),
            writes: Mutex::default(),
            version: AtomicUsize::new(2),
            connects: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
            pending: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
            hang_write: AtomicBool::new(false),
            hang_read: Mutex::default(),
            errors: Mutex::default(),
            ages: Mutex::default(),
            polls: AtomicUsize::new(0),
        })
    }
    fn set(&self, key: &str, value: Value) {
        self.values.lock().unwrap().insert(key.into(), value);
    }
    fn command(&self, member: String, parameters: Values) {
        match member.as_str() {
            "opencover" | "closecover" => {
                assert!(parameters.is_empty());
                self.set("coverstate", json!(2));
                self.set("covermoving", json!(true));
            }
            "haltcover" => {
                assert!(parameters.is_empty());
                self.set("coverstate", json!(4));
                self.set("covermoving", json!(false));
            }
            "calibratoron" => {
                assert_eq!(parameters.len(), 1);
                assert!(parameters["Brightness"].as_i64().is_some());
                self.set("brightness", parameters["Brightness"].clone());
                // Acknowledged illumination is still warming up.
                self.set("calibratorstate", json!(2));
                self.set("calibratorchanging", json!(true));
            }
            "calibratoroff" => {
                assert!(parameters.is_empty());
                self.set("brightness", json!(0));
                self.set("calibratorstate", json!(1));
                self.set("calibratorchanging", json!(false));
            }
            _ => panic!("Unexpected panel command {member}"),
        }
        self.writes.lock().unwrap().push((member, parameters));
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
    fn connection_info(&self) -> Option<ConnectionInfo> {
        let version = self.0.version.load(SeqCst) as u16;
        Some(ConnectionInfo {
            device_type: DeviceType::CoverCalibrator,
            interface_version: (version != 0).then_some(version),
            method: if version == 1 {
                ConnectionMethod::Legacy
            } else {
                ConnectionMethod::Async
            },
            owns_connection: true,
            uncertain: false,
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
            self.0.reads.lock().unwrap().push(member.clone());
            let hang = self.0.hang_read.lock().unwrap().as_ref() == Some(&member);
            if hang {
                std::future::pending::<()>().await;
            }
            self.0
                .values
                .lock()
                .unwrap()
                .get(&member)
                .cloned()
                .ok_or_else(|| SourceError::new(ErrorKind::Unsupported, "Private property absent"))
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
fn setup(device: &Arc<Device>) -> (Arc<SourceHandle>, CoverCalibratorController) {
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
    let controller =
        CoverCalibratorController::new(source.clone(), Duration::from_secs(2)).unwrap();
    (source, controller)
}
async fn settle() {
    for _ in 0..24 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn strict_wire_types_state_enums_and_signed_int32_bounds() {
    use Property::*;
    for (property, value) in [
        (Brightness, json!(-1)),
        (Brightness, json!(i64::MAX)),
        (Brightness, json!(1.0)),
        (Brightness, json!("0")),
        (MaxBrightness, json!(0)),
        (MaxBrightness, json!(false)),
        (CoverState, json!(-1)),
        (CoverState, json!(6)),
        (CalibratorState, json!(6)),
        (CoverMoving, json!(0)),
        (CalibratorChanging, json!("false")),
    ] {
        assert_eq!(
            property.decode(&value).unwrap_err().kind,
            ErrorKind::Unavailable
        );
    }
    for state in 0..=5 {
        CoverState.decode(&json!(state)).unwrap();
        CalibratorState.decode(&json!(state)).unwrap();
    }
    Brightness.decode(&json!(0)).unwrap();
    MaxBrightness.decode(&json!(i32::MAX)).unwrap();
    CoverMoving.decode(&json!(true)).unwrap();
    CalibratorChanging.decode(&json!(false)).unwrap();
}

#[tokio::test(start_paused = true)]
async fn siblings_observe_actual_independent_motion_and_light_without_disconnect_commands() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    assert_eq!(device.connects.load(SeqCst), 0);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    assert_eq!(device.connects.load(SeqCst), 1);
    assert_eq!(source.snapshot().lease_count, 2);
    assert_eq!(first.generation(), second.generation());
    first.open_cover().await.unwrap();
    first.calibrator_on(17).await.unwrap();
    for (property, value) in [
        (Property::CoverState, json!(2)),
        (Property::CoverMoving, json!(true)),
        (Property::CalibratorState, json!(2)),
        (Property::CalibratorChanging, json!(true)),
        (Property::Brightness, json!(17)),
    ] {
        assert_eq!(second.property(property).await.unwrap(), value);
    }
    drop(first);
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.disconnects.load(SeqCst), 0);
    device.set("calibratorstate", json!(3));
    device.set("calibratorchanging", json!(false));
    second.halt_cover().await.unwrap();
    assert_eq!(second.property(Property::CoverState).await.unwrap(), 4);
    assert_eq!(second.property(Property::CoverMoving).await.unwrap(), false);
    assert_eq!(second.property(Property::CalibratorState).await.unwrap(), 3);
    assert_eq!(second.property(Property::Brightness).await.unwrap(), 17);
    drop(second);
    settle().await;
    assert_eq!(device.disconnects.load(SeqCst), 1);
    assert_eq!(source.snapshot().lease_count, 0);
    assert_eq!(
        device
            .writes
            .lock()
            .unwrap()
            .iter()
            .map(|(m, _)| m.as_str())
            .collect::<Vec<_>>(),
        ["opencover", "calibratoron", "haltcover"]
    );
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn independent_absent_capabilities_do_not_probe_or_actuate_the_missing_component() {
    for (cover, light) in [(0, 1), (1, 0), (0, 0)] {
        let device = Device::new();
        device.set("coverstate", json!(cover));
        device.set("calibratorstate", json!(light));
        if light == 0 {
            device.values.lock().unwrap().remove("maxbrightness");
            device.values.lock().unwrap().remove("brightness");
        }
        let (source, controller) = setup(&device);
        let session = controller.connect().await.unwrap();
        let cap = session.capabilities().await.unwrap();
        assert_eq!(cap.cover_present, cover != 0);
        assert_eq!(cap.calibrator_present, light != 0);
        assert_eq!(
            cap.max_brightness,
            if light != 0 { Some(4096) } else { None }
        );
        if cover == 0 {
            for result in [
                session.open_cover().await,
                session.close_cover().await,
                session.halt_cover().await,
            ] {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Unsupported);
            }
        } else {
            session.open_cover().await.unwrap();
        }
        if light == 0 {
            for property in [Property::MaxBrightness, Property::Brightness] {
                assert_eq!(
                    session.property(property).await.unwrap_err().kind,
                    ErrorKind::Unsupported
                );
            }
            assert_eq!(
                session.calibrator_on(1).await.unwrap_err().kind,
                ErrorKind::Unsupported
            );
            assert_eq!(
                session.calibrator_off().await.unwrap_err().kind,
                ErrorKind::Unsupported
            );
            assert!(
                !device
                    .reads
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|m| m == "brightness" || m == "maxbrightness")
            );
        } else {
            session.calibrator_on(0).await.unwrap();
        }
        assert_eq!(
            device.writes.lock().unwrap().len(),
            usize::from(cover != 0) + usize::from(light != 0)
        );
        source.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn live_brightness_limit_off_zero_and_on_zero_are_distinct() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    let reads = device.reads.lock().unwrap().len();
    assert_eq!(
        session.calibrator_on(-1).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(device.reads.lock().unwrap().len(), reads);
    device.set("maxbrightness", json!(12));
    assert_eq!(
        session.calibrator_on(13).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    assert!(device.writes.lock().unwrap().is_empty());
    device.set("brightness", json!(1));
    assert_eq!(
        session
            .property(Property::Brightness)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    device.set("calibratorstate", json!(3));
    device.set("brightness", json!(13));
    assert_eq!(
        session
            .property(Property::Brightness)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    session.calibrator_on(0).await.unwrap();
    device.set("calibratorstate", json!(3));
    device.set("calibratorchanging", json!(false));
    assert_eq!(session.property(Property::Brightness).await.unwrap(), 0);
    assert_eq!(
        session.property(Property::CalibratorState).await.unwrap(),
        3
    );
    session.calibrator_off().await.unwrap();
    assert_eq!(
        session.property(Property::CalibratorState).await.unwrap(),
        1
    );
    assert_eq!(session.property(Property::Brightness).await.unwrap(), 0);
    for value in [json!(0), json!(i64::MAX), json!(1.5), json!("12")] {
        device.set("maxbrightness", value);
        assert_eq!(
            session.calibrator_on(1).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
    }
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn legacy_completion_uses_known_states_only_and_unknown_does_not_block_explicit_commands() {
    let device = Device::new();
    device.version.store(1, SeqCst);
    device.values.lock().unwrap().remove("covermoving");
    device.values.lock().unwrap().remove("calibratorchanging");
    device.set("coverstate", json!(4));
    device.set("calibratorstate", json!(5));
    let (source, controller) = setup(&device);
    let session = controller.connect().await.unwrap();
    for (state_member, property) in [
        ("coverstate", Property::CoverMoving),
        ("calibratorstate", Property::CalibratorChanging),
    ] {
        for state in 0..=5 {
            device.set(state_member, json!(state));
            if state >= 4 {
                assert_eq!(
                    session.property(property).await.unwrap_err().kind,
                    ErrorKind::Unavailable
                );
            } else {
                assert_eq!(session.property(property).await.unwrap(), state == 2);
            }
        }
    }
    session.open_cover().await.unwrap();
    session.calibrator_off().await.unwrap();
    assert!(
        !device
            .reads
            .lock()
            .unwrap()
            .iter()
            .any(|m| m == "covermoving" || m == "calibratorchanging")
    );
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn modern_or_unknown_version_requires_completion_properties_without_fallback() {
    for version in [0, 2] {
        for (member, value) in [
            ("covermoving", None),
            ("calibratorchanging", Some(json!(0))),
        ] {
            let device = Device::new();
            device.version.store(version, SeqCst);
            if let Some(value) = value {
                device.set(member, value);
            } else {
                device.values.lock().unwrap().remove(member);
            }
            let (source, controller) = setup(&device);
            let error = match controller.connect().await {
                Ok(_) => panic!("Invalid modern panel accepted"),
                Err(error) => error,
            };
            assert_eq!(
                error.kind,
                if member == "covermoving" {
                    ErrorKind::Unsupported
                } else {
                    ErrorKind::Unavailable
                }
            );
            settle().await;
            assert_eq!(source.snapshot().lease_count, 0);
            assert!(device.writes.lock().unwrap().is_empty());
            source.shutdown().await.unwrap();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn connection_deadline_and_cancelled_admission_release_without_actuation() {
    let device = Device::new();
    device.pending.store(true, SeqCst);
    let (source, controller) = setup(&device);
    let controller = Arc::new(controller);
    assert!(
        matches!(controller.connect().await, Err(error) if error.kind == ErrorKind::Unavailable)
    );
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    let task = tokio::spawn({
        let controller = controller.clone();
        async move { controller.connect().await }
    });
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    assert!(device.reads.lock().unwrap().is_empty());
    assert!(device.writes.lock().unwrap().is_empty());
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn preflight_transport_loss_prevents_dispatch_and_old_sessions_cannot_adopt_reconnection() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let first = controller.connect().await.unwrap();
    let generation = first.generation();
    *device.hang_read.lock().unwrap() = Some("maxbrightness".into());
    assert_eq!(
        first.calibrator_on(1).await.unwrap_err().kind,
        ErrorKind::Transient
    );
    assert!(device.writes.lock().unwrap().is_empty());
    assert_ne!(source.snapshot().generation, generation);
    *device.hang_read.lock().unwrap() = None;
    let fresh = controller.connect().await.unwrap();
    assert_ne!(fresh.generation(), generation);
    assert_eq!(
        first.open_cover().await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    fresh.calibrator_on(1).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn rejected_second_handshake_drops_only_its_lease_and_preserves_first_session() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    for timeout in [Duration::ZERO, Duration::from_secs(301)] {
        assert!(
            matches!(CoverCalibratorController::new(source.clone(), timeout), Err(error) if error.kind == ErrorKind::InvalidValue)
        );
    }
    let first = controller.connect().await.unwrap();
    device.set("calibratorchanging", json!("false"));
    assert!(
        matches!(controller.connect().await, Err(error) if error.kind == ErrorKind::Unavailable)
    );
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.disconnects.load(SeqCst), 0);
    assert!(first.connected());
    device.set("calibratorchanging", json!(false));
    first.open_cover().await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_preflight_never_dispatches_or_claims_write_uncertainty() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = Arc::new(controller.connect().await.unwrap());
    *device.hang_read.lock().unwrap() = Some("coverstate".into());
    let command = tokio::spawn({
        let session = session.clone();
        async move { session.open_cover().await }
    });
    settle().await;
    assert_eq!(source.snapshot().lease_count, 2);
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
async fn applied_command_with_lost_ack_fences_siblings_without_replay_or_automatic_darkening() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    device.uncertain.store(true, SeqCst);
    assert_eq!(
        first.calibrator_on(17).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    device.uncertain.store(false, SeqCst);
    assert!(!first.connected());
    assert!(!second.connected());
    for result in [
        second.calibrator_off().await,
        second.close_cover().await,
        second.halt_cover().await,
    ] {
        assert_eq!(result.unwrap_err().kind, ErrorKind::Uncertain);
    }
    assert!(source.snapshot().write_uncertain);
    assert_eq!(device.values.lock().unwrap()["brightness"], 17);
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    drop(first);
    settle().await;
    assert!(source.snapshot().write_uncertain);
    drop(second);
    settle().await;
    device.set("calibratorstate", json!(3));
    device.set("calibratorchanging", json!(false));
    let fresh = controller.connect().await.unwrap();
    assert_eq!(fresh.property(Property::Brightness).await.unwrap(), 17);
    fresh.calibrator_off().await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn concurrent_commands_share_control_and_cancelled_dispatch_retains_uncertainty() {
    let device = Device::new();
    let (source, controller) = setup(&device);
    let session = Arc::new(controller.connect().await.unwrap());
    let (a, b) = tokio::join!(session.open_cover(), session.calibrator_on(17));
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(a.err().or(b.err()).unwrap().kind, ErrorKind::Busy);
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    device.hang_write.store(true, SeqCst);
    let command = tokio::spawn({
        let session = session.clone();
        async move { session.calibrator_on(12).await }
    });
    settle().await;
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    command.abort();
    assert!(command.await.unwrap_err().is_cancelled());
    tokio::time::advance(Duration::from_millis(100)).await;
    settle().await;
    assert!(source.snapshot().write_uncertain);
    assert_eq!(
        session.halt_cover().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    source.shutdown().await.unwrap();
}

async fn alpaca_panel(version: u16, lose_ack: bool) {
    alpaca_panel_case(version, lose_ack, None).await;
}
async fn alpaca_panel_case(version: u16, lose_ack: bool, nested: Option<panel_virtual::Case>) {
    use axum::{
        Json, Router,
        body::to_bytes,
        extract::{Request, State},
        routing::any,
    };
    use regain_hub::{
        alpaca::AlpacaBackend,
        config::{ConnectionPolicy, SourceBackend, SourceConfig},
    };
    #[derive(Clone)]
    struct Fixture {
        device: Arc<Device>,
        connected: Arc<AtomicBool>,
        requests: Arc<Mutex<Vec<(String, String, Values)>>>,
        version: u16,
        lose_ack: bool,
    }
    async fn handler(State(state): State<Fixture>, request: Request) -> Json<Value> {
        assert!(
            request
                .uri()
                .path()
                .starts_with("/private/api/v1/covercalibrator/17/")
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
                    assert_eq!(state.version, 1);
                    assert_eq!(parameters.len(), 3);
                    state
                        .connected
                        .store(parameters["Connected"] == "true", SeqCst);
                }
                "connect" | "disconnect" => {
                    assert_eq!(state.version, 2);
                    assert_eq!(parameters.len(), 2);
                    state.connected.store(member == "connect", SeqCst);
                }
                _ => {
                    assert!(state.connected.load(SeqCst));
                    let args = if member == "calibratoron" {
                        assert_eq!(parameters.len(), 3);
                        Values::from([(
                            "Brightness".into(),
                            json!(
                                parameters["Brightness"]
                                    .as_str()
                                    .unwrap()
                                    .parse::<i32>()
                                    .unwrap()
                            ),
                        )])
                    } else {
                        assert_eq!(parameters.len(), 2);
                        Values::new()
                    };
                    state.device.command(member, args);
                    if state.lose_ack {
                        return Json(json!({"ErrorNumber":"invalid acknowledgement"}));
                    }
                }
            }
            Value::Null
        } else {
            match member.as_str() {
                "interfaceversion" => {
                    if state.device.pending.swap(false, SeqCst) {
                        tokio::time::sleep(Duration::from_millis(700)).await;
                    }
                    json!(state.version)
                }
                "connected" => json!(state.connected.load(SeqCst)),
                "connecting" => {
                    assert_eq!(state.version, 2);
                    json!(false)
                }
                _ => {
                    let hang = state.device.hang_read.lock().unwrap().as_ref() == Some(&member);
                    if hang {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                    if let Some(error) = state.device.errors.lock().unwrap().get(&member) {
                        return Json(json!({"ErrorNumber":error.upstream_code.unwrap_or(0x402),
                            "ClientTransactionID":parameters["ClientTransactionID"].as_str().unwrap().parse::<u32>().unwrap()}));
                    }
                    if state.version == 1
                        && matches!(member.as_str(), "covermoving" | "calibratorchanging")
                    {
                        return Json(
                            json!({"ErrorNumber":0x400,"ErrorMessage":"Legacy property absent",
                            "ClientTransactionID":parameters["ClientTransactionID"].as_str().unwrap().parse::<u32>().unwrap()}),
                        );
                    }
                    state.device.values.lock().unwrap()[&member].clone()
                }
            }
        };
        Json(
            json!({"ErrorNumber":0, "Value":value, "ClientTransactionID":parameters["ClientTransactionID"].as_str().unwrap().parse::<u32>().unwrap()}),
        )
    }
    let fixture = Fixture {
        device: Device::new(),
        connected: Arc::new(AtomicBool::new(false)),
        requests: Arc::new(Mutex::new(vec![])),
        version,
        lose_ack,
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
        label: "Private panel".into(),
        polling: PollPolicy {
            request_timeout_seconds: 1.0,
            connection_timeout_seconds: 3.0,
            ..PollPolicy::default()
        },
        backend: SourceBackend::Alpaca {
            base_url: format!("http://{address}/private/"),
            device_type: DeviceType::CoverCalibrator,
            device_number: 17,
            connection_policy: ConnectionPolicy::Managed,
            credential_reference: None,
        },
    };
    if let Some(case) = nested {
        panel_virtual::nested(
            config,
            fixture.device,
            fixture.connected,
            fixture.requests,
            version,
            lose_ack,
            case,
        )
        .await;
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        return;
    }
    use regain_hub::{
        config::{HubConfig, OutputConfig, VirtualDevice},
        diagnostics::{Diagnostics, Reading},
        runtime::HubRuntime,
        source::SourceRegistry,
    };
    let mut hub = HubConfig::empty();
    hub.sources.push(config.clone());
    for number in [4, 17] {
        hub.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Private panel {number}"),
            device: VirtualDevice::Proxy {
                source: config.id,
                device_type: DeviceType::CoverCalibrator,
            },
        });
    }
    let samples = regain_hub::factory::source_plans(&hub)
        .unwrap()
        .remove(&config.id)
        .unwrap()
        .samples;
    let clock = Arc::new(MonotonicClock::default());
    let registry = Arc::new(
        SourceRegistry::build(&hub, clock.clone(), |config| {
            Ok(Box::new(AlpacaBackend::new(config, samples.clone(), None)?))
        })
        .unwrap(),
    );
    let source = registry.get(config.id).unwrap();
    let runtime = HubRuntime::from_registry(hub.clone(), registry, clock).unwrap();
    let a = runtime.client();
    let b = runtime.client();
    a.connect(hub.outputs[0].id).await.unwrap();
    b.connect(hub.outputs[1].id).await.unwrap();
    let first_connection = a.connection(hub.outputs[0].id).unwrap();
    let second_connection = b.connection(hub.outputs[1].id).unwrap();
    let first = first_connection.covercalibrator().unwrap();
    let second = second_connection.covercalibrator().unwrap();
    let info = source.snapshot().connection_info.unwrap();
    assert_eq!(info.interface_version, Some(version));
    assert!(info.owns_connection);
    assert_eq!(
        info.method,
        if version == 1 {
            ConnectionMethod::Legacy
        } else {
            ConnectionMethod::Async
        }
    );
    if lose_ack {
        assert_eq!(
            first.calibrator_on(17).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            second.calibrator_off().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            second.close_cover().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert!(source.snapshot().write_uncertain);
        assert!(!second.connected());
    } else {
        first.open_cover().await.unwrap();
        assert_eq!(second.property(Property::CoverState).await.unwrap(), 2);
        assert_eq!(second.property(Property::CoverMoving).await.unwrap(), true);
        second.halt_cover().await.unwrap();
        assert_eq!(first.property(Property::CoverState).await.unwrap(), 4);
        if version == 1 {
            assert_eq!(
                first
                    .property(Property::CoverMoving)
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::Unavailable
            );
        } else {
            assert_eq!(first.property(Property::CoverMoving).await.unwrap(), false);
        }
        first.calibrator_on(0).await.unwrap();
        assert_eq!(second.property(Property::CalibratorState).await.unwrap(), 2);
        assert_eq!(
            second.property(Property::CalibratorChanging).await.unwrap(),
            true
        );
        fixture.device.set("calibratorstate", json!(3));
        fixture.device.set("calibratorchanging", json!(false));
        assert_eq!(second.property(Property::Brightness).await.unwrap(), 0);
        assert_eq!(second.property(Property::CalibratorState).await.unwrap(), 3);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let Diagnostics::CoverCalibrator {properties,..} = runtime.output_status(hub.outputs[1].id,0,32).unwrap().diagnostics else {panic!()};
                let state = serde_json::to_value(&properties).unwrap();
                // Polling publishes one property at a time. State arrival does
                // not establish that the brightness-bound dependency arrived.
                if state[2]["sample"]["reading"]["value"]["value"] == 4 && state[3]["sample"]["reading"]["value"]["value"] == 3
                    && source.snapshot().values.get("maxbrightness") == Some(&json!(4096)) {
                    assert!(matches!(properties[0].sample,Reading::Available { .. }), "{}", serde_json::to_string(&source.snapshot()).unwrap());
                    if version == 1 { assert!(matches!(&properties[4].sample,Reading::Unavailable {error} if error.kind == ErrorKind::Unavailable)); }
                    else { assert!(matches!(properties[4].sample,Reading::Available { .. })); }
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.unwrap();
    }
    drop(first_connection);
    a.close();
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    if !lose_ack {
        assert!(fixture.connected.load(SeqCst));
    }
    drop(second_connection);
    b.close();
    runtime.shutdown().await.unwrap();
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
    let connections = requests
        .iter()
        .filter(|(method, member, parameters)| {
            method == "PUT"
                && (member == "connect"
                    || member == "connected" && parameters["Connected"] == "true")
        })
        .count();
    assert_eq!(connections, 1);
    assert_eq!(
        fixture.device.writes.lock().unwrap().len(),
        if lose_ack { 1 } else { 3 }
    );
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
#[tokio::test]
async fn real_alpaca_v1_derives_only_known_completion_and_shares_connection() {
    alpaca_panel(1, false).await;
}
#[tokio::test]
async fn real_alpaca_v2_retains_async_motion_and_zero_on_with_shared_connection() {
    alpaca_panel(2, false).await;
}
#[tokio::test]
async fn real_alpaca_lost_ack_applies_once_and_never_replays_or_darkens() {
    alpaca_panel(2, true).await;
}

#[path = "support/covercalibrator_runtime.rs"]
mod panel_runtime;

#[path = "support/covercalibrator_virtual.rs"]
mod panel_virtual;
