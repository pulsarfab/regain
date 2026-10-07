use regain_hub::{
    config::{ConfigStore, DeviceType, HubConfig},
    runtime::HubRuntime,
    safety::MonotonicClock,
    service::{HubService, UpdateError},
    source::{Backend, BackendFuture, ErrorKind, SourceError, SourceRegistry, Values},
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

#[derive(Default)]
struct Device {
    connects: AtomicUsize,
    reads: Mutex<Vec<(String, Values)>>,
    hang: AtomicBool,
    resume: tokio::sync::Notify,
    lost: AtomicBool,
    invalid_safety: AtomicBool,
    pending_connect: AtomicBool,
    retry_after: AtomicBool,
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
            if self.0.pending_connect.load(SeqCst) {
                Ok(false)
            } else {
                self.connect().await?;
                Ok(true)
            }
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn reset(&mut self) {}
    fn write(&mut self, _: String, _: Values) -> BackendFuture<'_, Value> {
        panic!("Inspection must never write equipment commands")
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async { Ok(Values::from([("issafe".into(), json!(true))])) })
    }
    fn read(&mut self, member: String, args: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.0
                .reads
                .lock()
                .unwrap()
                .push((member.clone(), args.clone()));
            if self.0.hang.load(SeqCst) {
                self.0.resume.notified().await;
            }
            if self.0.lost.load(SeqCst) && member == "minswitchvalue" {
                return Err(SourceError::timeout());
            }
            if self.0.retry_after.load(SeqCst) {
                return Err(SourceError {
                    retry_after: Some(Duration::from_secs(2)),
                    ..SourceError::transient()
                });
            }
            Ok(match member.as_str() {
                "maxswitch" => json!(20),
                "getswitchname" => json!(format!("Channel {}", args["Id"])),
                "getswitchdescription" => json!("Fixture channel"),
                "canwrite" => json!(args["Id"].as_u64().unwrap().is_multiple_of(2)),
                "minswitchvalue" => json!(0),
                "maxswitchvalue" => json!(10),
                "switchstep" => {
                    if args["Id"] == 4 {
                        json!(3)
                    } else {
                        json!(1)
                    }
                }
                "issafe" => {
                    if self.0.invalid_safety.load(SeqCst) {
                        json!(1)
                    } else {
                        json!(true)
                    }
                }
                "sensordescription" if args["SensorName"] == "humidity" => {
                    return Err(SourceError::new(
                        ErrorKind::Unsupported,
                        "Fixture sensor not implemented",
                    ));
                }
                "sensordescription" => json!("Fixture weather sensor"),
                "timesincelastupdate" => json!(1.5),
                "humidity" => {
                    return Err(SourceError::new(
                        ErrorKind::Unavailable,
                        "Fixture sensor temporarily unavailable",
                    ));
                }
                "rainrate" => Value::Null,
                _ => json!(20),
            })
        })
    }
}
fn configuration(kind: DeviceType) -> HubConfig {
    let mut config = HubConfig::empty();
    config.sources.push(serde_json::from_value(json!({"id":Uuid::new_v4(),"label":"Inspection fixture",
        "backend":{"kind":"alpaca","baseUrl":"http://127.0.0.1:1/","deviceType":kind,"deviceNumber":0}})).unwrap());
    config.sources[0].polling.request_timeout_seconds = 5.0;
    config
}
fn runtime(config: HubConfig, device: Arc<Device>) -> Arc<HubRuntime> {
    let clock = Arc::new(MonotonicClock::default());
    let registry = SourceRegistry::build(&config, clock.clone(), |_| {
        Ok(Box::new(Mock(device.clone())))
    })
    .unwrap();
    HubRuntime::from_registry(config, Arc::new(registry), clock).unwrap()
}
#[tokio::test]
async fn switch_pages_keep_real_ids_permissions_and_use_the_write_grid_validator() {
    let config = configuration(DeviceType::Switch);
    let source = config.sources[0].id;
    let device = Arc::new(Device::default());
    let hub = runtime(config.clone(), device.clone());
    let report = serde_json::to_value(hub.inspect_source(source, 3, 2).await.unwrap()).unwrap();
    assert_eq!(report["configurationRevision"], config.revision.to_string());
    let caps = &report["capabilities"];
    assert_eq!(caps["count"]["value"], 20);
    assert_eq!(caps["nextStart"], 5);
    assert_eq!(caps["channels"][0]["id"], 3);
    assert_eq!(caps["channels"][0]["canWrite"]["value"], false);
    assert_eq!(caps["channels"][0]["rangeValid"], true);
    assert_eq!(caps["channels"][1]["id"], 4);
    assert_eq!(caps["channels"][1]["canWrite"]["value"], true);
    assert_eq!(caps["channels"][1]["rangeValid"], false);
    assert_eq!(device.reads.lock().unwrap().len(), 13);
    assert_eq!(hub.active_connections(), 0);
    hub.shutdown().await.unwrap();
}
#[tokio::test]
async fn weather_reports_unsupported_unavailable_invalid_and_observed_separately() {
    let config = configuration(DeviceType::ObservingConditions);
    let source = config.sources[0].id;
    let hub = runtime(config, Arc::new(Device::default()));
    let report = serde_json::to_value(hub.inspect_source(source, 0, 4).await.unwrap()).unwrap();
    let measurements = report["capabilities"]["measurements"].as_array().unwrap();
    assert_eq!(measurements.len(), 13);
    let humidity = measurements
        .iter()
        .find(|p| p["property"] == "humidity")
        .unwrap();
    assert_eq!(humidity["description"]["state"], "unsupported");
    assert_eq!(humidity["reading"]["state"], "unavailable");
    assert_eq!(humidity["unit"], "%");
    let rain = measurements
        .iter()
        .find(|p| p["property"] == "rainrate")
        .unwrap();
    assert_eq!(rain["reading"]["state"], "unavailable");
    let temperature = measurements
        .iter()
        .find(|p| p["property"] == "temperature")
        .unwrap();
    assert_eq!(temperature["reading"]["state"], "observed");
    assert_eq!(temperature["ageSeconds"]["value"], 1.5);
    assert_eq!(temperature["unit"], "°C");
    hub.shutdown().await.unwrap();
}
#[tokio::test]
async fn bad_pages_and_unknown_sources_do_no_io_and_safety_requires_a_boolean() {
    let config = configuration(DeviceType::SafetyMonitor);
    let source = config.sources[0].id;
    let device = Arc::new(Device::default());
    device.invalid_safety.store(true, SeqCst);
    let hub = runtime(config, device.clone());
    for (source, start, limit) in [
        (source, 1, 4),
        (source, 0, 0),
        (source, 0, 9),
        (Uuid::new_v4(), 0, 4),
    ] {
        assert_eq!(
            hub.inspect_source(source, start, limit)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(device.connects.load(SeqCst), 0);
    let report = serde_json::to_value(hub.inspect_source(source, 0, 4).await.unwrap()).unwrap();
    assert_eq!(report["capabilities"]["isSafe"]["state"], "unavailable");
    hub.shutdown().await.unwrap();
}
#[tokio::test]
async fn generation_loss_discards_partial_metadata_and_retry_after_stops_the_scan() {
    for retry_after in [false, true] {
        let config = configuration(DeviceType::Switch);
        let source = config.sources[0].id;
        let device = Arc::new(Device::default());
        device.lost.store(!retry_after, SeqCst);
        device.retry_after.store(retry_after, SeqCst);
        let hub = runtime(config, device.clone());
        let error = hub.inspect_source(source, 0, 4).await.unwrap_err();
        if retry_after {
            assert_eq!(error.retry_after, Some(Duration::from_secs(2)));
            let reply = serde_json::to_value(regain_hub::ipc::RpcError::from(error)).unwrap();
            assert_eq!(reply["retryAfterSeconds"], 2.0);
            assert_eq!(device.reads.lock().unwrap().len(), 1);
        } else {
            assert!(error.message.contains("connection changed"));
            assert!(
                !device
                    .reads
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(name, _)| name == "maxswitchvalue")
            );
        }
        hub.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn active_inspection_blocks_apply_and_cancellation_releases_its_temporary_lease() {
    let config = configuration(DeviceType::Switch);
    let source = config.sources[0].id;
    let device = Arc::new(Device::default());
    device.hang.store(true, SeqCst);
    let builder_device = device.clone();
    let service = HubService::persistent(
        ConfigStore::new(None, config).unwrap(),
        Arc::new(move |config| Ok(runtime(config, builder_device.clone()))),
    )
    .unwrap();
    let hub = service.runtime().unwrap();
    let task = tokio::spawn({
        let hub = hub.clone();
        async move { hub.inspect_source(source, 0, 4).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while device.reads.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(hub.active_connections(), 1);
    let config = service.configuration();
    assert!(matches!(
        service.apply(config.revision, config.clone()).await,
        Err(UpdateError::Connected)
    ));
    task.abort();
    let _ = task.await;
    assert_eq!(hub.active_connections(), 0);
    device.hang.store(false, SeqCst);
    device.resume.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        while hub.source_snapshot(source).unwrap().lease_count != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(service.apply(config.revision, config).await.unwrap().ready);
    assert_eq!(
        hub.inspect_source(source, 0, 4).await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    service.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn total_inspection_deadline_bounds_a_pending_connection() {
    let config = configuration(DeviceType::Switch);
    let source = config.sources[0].id;
    let device = Arc::new(Device::default());
    device.pending_connect.store(true, SeqCst);
    let hub = runtime(config, device);
    let start = tokio::time::Instant::now();
    let error = hub.inspect_source(source, 0, 4).await.unwrap_err();
    assert!(error.message.contains("inspection deadline"));
    assert_eq!(
        start.elapsed(),
        regain_hub::capabilities::INSPECTION_TIMEOUT
    );
    assert_eq!(hub.active_connections(), 0);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_alpaca_inspections_share_owned_connection_and_only_release_their_lease() {
    use axum::{
        Json, Router,
        extract::{Path, State},
        routing::get,
    };
    #[derive(Default)]
    struct Remote {
        connected: AtomicBool,
        opens: AtomicUsize,
        closes: AtomicUsize,
    }
    async fn read(State(state): State<Arc<Remote>>, Path(member): Path<String>) -> Json<Value> {
        let value = match member.as_str() {
            "interfaceversion" => json!(3),
            "connected" => json!(state.connected.load(SeqCst)),
            "connecting" => json!(false),
            "maxswitch" => json!(2),
            "canwrite" => json!(true),
            "getswitchname" => json!("Real HTTP fixture"),
            "getswitchdescription" => json!("Fixture only"),
            "minswitchvalue" => json!(0),
            "maxswitchvalue" => json!(10),
            "switchstep" => json!(1),
            _ => panic!("Unexpected read {member}"),
        };
        Json(json!({"Value":value,"ErrorNumber":0}))
    }
    async fn write(State(state): State<Arc<Remote>>, Path(member): Path<String>) -> Json<Value> {
        match member.as_str() {
            "connect" => {
                state.opens.fetch_add(1, SeqCst);
                state.connected.store(true, SeqCst);
            }
            "disconnect" => {
                state.closes.fetch_add(1, SeqCst);
                state.connected.store(false, SeqCst);
            }
            _ => panic!("Inspection wrote equipment command {member}"),
        }
        Json(json!({"ErrorNumber":0}))
    }
    let state = Arc::new(Remote::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new()
        .route("/api/v1/switch/0/{member}", get(read).put(write))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut config = configuration(DeviceType::Switch);
    let source = config.sources[0].id;
    if let regain_hub::config::SourceBackend::Alpaca {
        base_url,
        connection_policy,
        ..
    } = &mut config.sources[0].backend
    {
        *base_url = format!("http://{address}/");
        *connection_policy = regain_hub::config::ConnectionPolicy::Managed;
    }
    let clock = Arc::new(MonotonicClock::default());
    let registry = regain_hub::factory::build_sources(
        &config,
        &regain_hub::native::NativeRuntime {
            cameras: None,
            directory: "unused".into(),
            simulate: false,
            references: None,
        },
        &regain_hub::factory::NoCredentials,
        clock.clone(),
    )
    .unwrap();
    let retained = regain_hub::readout::SourceLease::acquire(registry.get(source).unwrap())
        .await
        .unwrap();
    let hub = HubRuntime::from_registry(config, registry, clock).unwrap();
    let (a, b) = tokio::join!(
        hub.inspect_source(source, 0, 1),
        hub.inspect_source(source, 1, 1)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.generation, b.generation);
    assert_eq!(a.connection.unwrap().interface_version, Some(3));
    assert!(b.connection.unwrap().owns_connection);
    assert_eq!(state.opens.load(SeqCst), 1);
    assert_eq!(state.closes.load(SeqCst), 0);
    assert!(state.connected.load(SeqCst));
    drop(retained);
    hub.shutdown().await.unwrap();
    assert_eq!(state.closes.load(SeqCst), 1);
    server.abort();
    let _ = server.await;
}
