//! Real factory/controller composition over private loopback hardware doubles.
use regain_hub::{
    config::{DeviceType, HubConfig, OutputConfig, SourceBackend, SourceConfig, VirtualDevice},
    factory::NoCredentials,
    native::NativeRuntime,
    parameters::PollPolicy,
    runtime::HubRuntime,
    safety::MonotonicClock,
    source::{ErrorKind, Values},
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
    delay_move: AtomicBool,
    bad_motion: AtomicBool,
    delay_connection: AtomicBool,
    opens: AtomicUsize,
    version_reads: AtomicUsize,
}
struct Fixture {
    device: Arc<Device>,
    config: HubConfig,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(relative: bool) -> Self {
        use axum::{
            Json,
            body::to_bytes,
            extract::{Request, State},
        };
        async fn handler(State(device): State<Arc<Device>>, request: Request) -> Json<Value> {
            let method = request.method().clone();
            let member = request.uri().path().rsplit('/').next().unwrap().to_owned();
            let body = if method == "GET" {
                request
                    .uri()
                    .query()
                    .unwrap_or_default()
                    .as_bytes()
                    .to_vec()
            } else {
                to_bytes(request.into_body(), 4096).await.unwrap().to_vec()
            };
            let parameters = url::form_urlencoded::parse(&body)
                .map(|(key, value)| (key.into_owned(), json!(value)))
                .collect::<Values>();
            if method == "PUT" {
                if member == "connected" {
                    if parameters["Connected"] == "true" {
                        device.opens.fetch_add(1, SeqCst);
                    }
                    device
                        .values
                        .lock()
                        .unwrap()
                        .insert(member, json!(parameters["Connected"] == "true"));
                } else {
                    device
                        .writes
                        .lock()
                        .unwrap()
                        .push((member.clone(), parameters.clone()));
                    match member.as_str() {
                        "move" => {
                            let position = parameters["Position"]
                                .as_str()
                                .unwrap()
                                .parse::<i32>()
                                .unwrap();
                            let mut values = device.values.lock().unwrap();
                            values.insert("position".into(), json!(position));
                            values.insert("ismoving".into(), json!(true));
                        }
                        "halt" => {
                            device
                                .values
                                .lock()
                                .unwrap()
                                .insert("ismoving".into(), json!(false));
                        }
                        "tempcomp" => {
                            device
                                .values
                                .lock()
                                .unwrap()
                                .insert("tempcomp".into(), json!(parameters["TempComp"] == "true"));
                        }
                        _ => panic!("Unexpected private command {member}"),
                    }
                    if member == "move" && device.delay_move.load(SeqCst) {
                        tokio::time::sleep(Duration::from_millis(700)).await;
                    }
                }
                return Json(json!({"ErrorNumber":0,"Value":null}));
            }
            if member == "interfaceversion" {
                device.version_reads.fetch_add(1, SeqCst);
                if device.delay_connection.load(SeqCst) {
                    tokio::time::sleep(Duration::from_millis(700)).await;
                }
            }
            let values = device.values.lock().unwrap();
            let result = if member == "ismoving" && device.bad_motion.load(SeqCst) {
                json!("false")
            } else if member == "stepsize" || member == "position" && values["absolute"] == false {
                return Json(json!({"ErrorNumber":1024,"ErrorMessage":"private detail"}));
            } else {
                values.get(&member).cloned().unwrap_or(Value::Null)
            };
            Json(json!({"ErrorNumber":0,"Value":result}))
        }
        let device = Arc::new(Device {
            values: Mutex::new(Values::from([
                ("absolute".into(), json!(!relative)),
                ("maxstep".into(), json!(100000)),
                ("maxincrement".into(), json!(1000)),
                ("tempcompavailable".into(), json!(true)),
                ("position".into(), json!(50000)),
                ("ismoving".into(), json!(false)),
                ("tempcomp".into(), json!(false)),
                ("temperature".into(), json!(-5.0)),
                ("interfaceversion".into(), json!(3)),
                ("connected".into(), json!(false)),
            ])),
            writes: Mutex::default(),
            delay_move: AtomicBool::new(false),
            bad_motion: AtomicBool::new(false),
            delay_connection: AtomicBool::new(false),
            opens: AtomicUsize::new(0),
            version_reads: AtomicUsize::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new()
            .fallback(axum::routing::any(handler))
            .with_state(device.clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        let mut config = HubConfig::empty();
        let leaf = Uuid::new_v4();
        config.sources.push(SourceConfig {
            id: leaf, label: "Private loopback focuser".into(),
            polling: PollPolicy { poll_seconds: 0.1, request_timeout_seconds: 0.4, connection_timeout_seconds: 2.0, ..Default::default() },
            backend: serde_json::from_value(json!({"kind":"alpaca","baseUrl":format!("http://{address}/"),"deviceType":"focuser","deviceNumber":0,"connectionPolicy":"managed"})).unwrap(),
        });
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number: 0,
            label: "Leaf output".into(),
            device: VirtualDevice::Proxy {
                source: leaf,
                device_type: DeviceType::Focuser,
            },
        });
        for number in [4, 7] {
            let source = Uuid::new_v4();
            config.sources.push(SourceConfig {
                id: source,
                label: "Local focuser input".into(),
                polling: PollPolicy {
                    poll_seconds: 0.1,
                    request_timeout_seconds: 1.0,
                    connection_timeout_seconds: 2.0,
                    ..Default::default()
                },
                backend: SourceBackend::Virtual {
                    output: config.outputs.last().unwrap().id,
                },
            });
            config.outputs.push(OutputConfig {
                id: Uuid::new_v4(),
                number,
                label: "Nested focuser".into(),
                device: VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::Focuser,
                },
            });
        }
        Self {
            device,
            config,
            stop: Some(stop),
            server,
        }
    }
    fn build(&self) -> Arc<HubRuntime> {
        HubRuntime::build(
            self.config.clone(),
            &NativeRuntime {
                directory: "no-workers".into(),
                simulate: false,
                references: None,
            },
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap()
    }
    fn count(&self, member: &str) -> usize {
        self.device
            .writes
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == member)
            .count()
    }
    async fn finish(mut self, hub: Arc<HubRuntime>) {
        hub.shutdown().await.unwrap();
        assert!(
            hub.source_snapshots()
                .iter()
                .all(|source| source.lease_count == 0)
        );
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(3), &mut self.server)
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn eventually(mut test: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn nested_focusers_share_typed_motion_limits_optional_errors_and_private_leases() {
    let fixture = Fixture::new(false).await;
    let hub = fixture.build();
    assert!(
        hub.source_snapshots()
            .iter()
            .all(|source| source.lease_count == 0)
    );
    assert!(hub.outputs().iter().all(|output| !output.simulated));
    let direct = hub.client();
    let outer = hub.client();
    let base = fixture.config.outputs[0].id;
    let output = fixture.config.outputs[2].id;
    let result = direct.connect(base).await;
    assert!(
        result.is_ok(),
        "{result:?}; {:?}; {:?}",
        hub.source_snapshots(),
        fixture.device.values.lock().unwrap()
    );
    let result = outer.connect(output).await;
    assert!(result.is_ok(), "{result:?}; {:?}", hub.source_snapshots());
    let a = direct.connection(base).unwrap();
    let b = outer.connection(output).unwrap();
    assert_eq!(
        b.focuser().unwrap().capabilities().await.unwrap().max_step,
        100000
    );
    assert_eq!(b.focuser().unwrap().temperature().await.unwrap(), -5.0);
    assert_eq!(
        b.focuser().unwrap().step_size().await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        b.focuser().unwrap().move_to(52000).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(fixture.count("move"), 0);
    b.focuser().unwrap().move_to(50100).await.unwrap();
    assert_eq!(a.focuser().unwrap().position().await.unwrap(), 50100);
    assert!(a.focuser().unwrap().is_moving().await.unwrap());
    assert_eq!(
        a.focuser().unwrap().move_to(50200).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    a.focuser().unwrap().halt().await.unwrap();
    b.focuser().unwrap().set_temp_comp(true).await.unwrap();
    assert!(a.focuser().unwrap().temp_comp().await.unwrap());
    assert_eq!(fixture.count("move"), 1);
    assert_eq!(fixture.device.opens.load(SeqCst), 1);
    outer.close();
    drop(b);
    eventually(|| {
        hub.source_snapshot(fixture.config.sources[2].id)
            .unwrap()
            .lease_count
            == 0
    })
    .await;
    assert!(a.focuser().unwrap().connected());
    assert_eq!(a.focuser().unwrap().position().await.unwrap(), 50100);
    direct.close();
    drop(a);
    drop(direct);
    drop(outer);
    fixture.finish(hub).await;
}

#[tokio::test]
async fn nested_relative_focusers_preserve_signed_distances_without_inventing_position() {
    let fixture = Fixture::new(true).await;
    let hub = fixture.build();
    let client = hub.client();
    let output = fixture.config.outputs[2].id;
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    let focuser = connection.focuser().unwrap();
    assert!(!focuser.capabilities().await.unwrap().absolute);
    assert_eq!(
        focuser.position().await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        focuser.move_to(i32::MIN).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    focuser.move_to(-30).await.unwrap();
    assert_eq!(
        fixture.device.writes.lock().unwrap()[0].1["Position"],
        "-30"
    );
    focuser.halt().await.unwrap();
    eventually(|| {
        hub.source_snapshot(fixture.config.sources[2].id)
            .unwrap()
            .sample_errors
            .get("position")
            .is_some_and(|error| error.kind == ErrorKind::Unsupported)
    })
    .await;
    assert!(
        !hub.source_snapshot(fixture.config.sources[2].id)
            .unwrap()
            .values
            .contains_key("position")
    );
    client.close();
    drop(connection);
    drop(client);
    fixture.finish(hub).await;
}

#[tokio::test]
async fn virtual_focuser_polling_preserves_cached_age_and_individual_property_errors() {
    let mut fixture = Fixture::new(false).await;
    fixture.config.sources[0].polling.poll_seconds = 300.0;
    let hub = fixture.build();
    let client = hub.client();
    client.connect(fixture.config.outputs[2].id).await.unwrap();
    let leaf = fixture.config.sources[0].id;
    let outer = fixture.config.sources[2].id;
    eventually(|| {
        hub.source_snapshot(leaf)
            .unwrap()
            .values
            .contains_key("temperature")
    })
    .await;
    let sequence = hub.source_snapshot(leaf).unwrap().sequence;
    eventually(|| {
        hub.source_snapshot(outer)
            .unwrap()
            .sample_ages_seconds
            .get("temperature")
            .is_some_and(|age| *age > 0.15)
    })
    .await;
    assert_eq!(hub.source_snapshot(leaf).unwrap().sequence, sequence);
    let state = hub.source_snapshot(outer).unwrap();
    assert_eq!(state.values["maxstep"], 100000);
    assert_eq!(state.values["ismoving"], false);
    assert_eq!(state.sample_errors["stepsize"].kind, ErrorKind::Unsupported);
    assert!(!state.values.contains_key("stepsize"));
    client.close();
    drop(client);
    fixture.finish(hub).await;
}

#[tokio::test]
async fn uncertain_nested_move_is_dispatched_once_and_cannot_rebind_an_old_session() {
    let fixture = Fixture::new(false).await;
    let hub = fixture.build();
    let direct = hub.client();
    let outer = hub.client();
    direct.connect(fixture.config.outputs[0].id).await.unwrap();
    outer.connect(fixture.config.outputs[2].id).await.unwrap();
    let a = direct.connection(fixture.config.outputs[0].id).unwrap();
    let b = outer.connection(fixture.config.outputs[2].id).unwrap();
    fixture.device.delay_move.store(true, SeqCst);
    assert_eq!(
        b.focuser().unwrap().move_to(50100).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(fixture.count("move"), 1);
    assert_eq!(
        a.focuser().unwrap().move_to(50200).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    eventually(|| !b.focuser().unwrap().connected()).await;
    outer.connect(fixture.config.outputs[2].id).await.unwrap();
    assert!(!b.focuser().unwrap().connected());
    assert!(b.focuser().unwrap().position().await.is_err());
    assert_eq!(fixture.count("halt"), 0);
    assert_eq!(fixture.count("move"), 1);
    assert!(
        hub.source_snapshot(fixture.config.sources[0].id)
            .unwrap()
            .write_uncertain
    );
    direct.close();
    outer.close();
    drop(a);
    drop(b);
    drop(direct);
    drop(outer);
    fixture.finish(hub).await;
}

#[tokio::test]
async fn pending_inner_connection_outlives_an_outer_request_step_without_repeated_activation() {
    let mut fixture = Fixture::new(false).await;
    fixture.device.delay_connection.store(true, SeqCst);
    fixture.config.sources[0].polling.request_timeout_seconds = 1.0;
    for source in &mut fixture.config.sources[1..] {
        source.polling.request_timeout_seconds = 0.1;
    }
    let hub = fixture.build();
    let client = hub.client();
    let started = tokio::time::Instant::now();
    client.connect(fixture.config.outputs[2].id).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(700));
    assert_eq!(fixture.device.opens.load(SeqCst), 1);
    assert_eq!(fixture.device.version_reads.load(SeqCst), 1);
    let connection = client.connection(fixture.config.outputs[2].id).unwrap();
    assert!(connection.focuser().unwrap().connected());
    assert_eq!(
        connection.focuser().unwrap().position().await.unwrap(),
        50000
    );
    assert_eq!(fixture.count("move"), 0);
    assert!(
        hub.source_snapshots()
            .iter()
            .all(|source| source.transport_connected)
    );
    client.close();
    drop(connection);
    drop(client);
    fixture.finish(hub).await;
}

#[tokio::test]
async fn invalid_nested_motion_read_is_an_error_and_cannot_dispatch_a_move() {
    let fixture = Fixture::new(false).await;
    let hub = fixture.build();
    let client = hub.client();
    client.connect(fixture.config.outputs[2].id).await.unwrap();
    let connection = client.connection(fixture.config.outputs[2].id).unwrap();
    fixture.device.bad_motion.store(true, SeqCst);
    assert_eq!(
        connection
            .focuser()
            .unwrap()
            .is_moving()
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        connection
            .focuser()
            .unwrap()
            .move_to(50100)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(fixture.count("move"), 0);
    let source = fixture.config.sources[2].id;
    eventually(|| {
        hub.source_snapshot(source)
            .unwrap()
            .sample_errors
            .get("ismoving")
            // The Alpaca sampling adapter rejects a malformed wire value as
            // Permanent; virtual caches preserve that original classification.
            .is_some_and(|error| error.kind == ErrorKind::Permanent)
    })
    .await;
    assert!(
        !hub.source_snapshot(source)
            .unwrap()
            .values
            .contains_key("ismoving")
    );
    assert_eq!(
        connection.focuser().unwrap().position().await.unwrap(),
        50000
    );
    assert!(connection.focuser().unwrap().connected());
    client.close();
    drop(connection);
    drop(client);
    fixture.finish(hub).await;
}

#[tokio::test]
async fn cancelling_pending_nested_focuser_connection_releases_supervised_inner_clients() {
    let mut fixture = Fixture::new(false).await;
    fixture.device.delay_connection.store(true, SeqCst);
    fixture.config.sources[0].polling.request_timeout_seconds = 1.0;
    let hub = fixture.build();
    let client = hub.client();
    let output = fixture.config.outputs[2].id;
    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.connect(output).await }
    });
    eventually(|| {
        hub.source_snapshot(fixture.config.sources[0].id)
            .unwrap()
            .lease_count
            > 0
    })
    .await;
    assert!(client.connection(output).is_err());
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    client.close();
    drop(client);
    eventually(|| {
        hub.active_connections() == 0
            && hub
                .source_snapshots()
                .iter()
                .all(|source| source.lease_count == 0)
    })
    .await;
    assert_eq!(fixture.count("move"), 0);
    assert_eq!(fixture.count("halt"), 0);
    fixture.finish(hub).await;
}

#[tokio::test]
async fn native_worker_simulation_remains_explicit_through_nested_focuser_outputs() {
    let Some(directory) = std::env::var_os("REGAIN_TEST_WORKERS") else {
        eprintln!("Native virtual focuser test requires built workers via REGAIN_TEST_WORKERS");
        return;
    };
    let mut fixture = Fixture::new(false).await;
    fixture.config.sources[0].backend = SourceBackend::Native {
        device: regain_hub::config::NativeDevice::Eaf,
        identity: "0102030405060709".into(),
        filter_wheel: None,
    };
    for source in &mut fixture.config.sources {
        source.polling.request_timeout_seconds = 5.0;
        source.polling.connection_timeout_seconds = 10.0;
    }
    let hub = HubRuntime::build(
        fixture.config.clone(),
        &NativeRuntime {
            directory: directory.into(),
            simulate: true,
            references: None,
        },
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    assert!(hub.outputs().iter().all(|output| output.simulated));
    assert!(hub.source_snapshots().iter().all(|source| source.simulated));
    let client = hub.client();
    client.connect(fixture.config.outputs[2].id).await.unwrap();
    let connection = client.connection(fixture.config.outputs[2].id).unwrap();
    let initial = connection.focuser().unwrap().position().await.unwrap();
    let target = initial + 20;
    connection.focuser().unwrap().move_to(target).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while connection.focuser().unwrap().is_moving().await.unwrap() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        connection.focuser().unwrap().position().await.unwrap(),
        target
    );
    assert_eq!(
        connection
            .focuser()
            .unwrap()
            .step_size()
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(fixture.device.opens.load(SeqCst), 0);
    client.close();
    drop(connection);
    drop(client);
    fixture.finish(hub).await;
}
