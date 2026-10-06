//! Shared output/client lifecycle tests with explicit fault injection.
use regain_hub::{
    config::{HubConfig, Measurement, OutputConfig, Readout, VirtualDevice, WeatherMetric},
    runtime::HubRuntime,
    safety::MonotonicClock,
    source::{Backend, BackendFuture, ErrorKind, SourceRegistry, Values},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Default)]
struct Device {
    connects: AtomicUsize,
    disconnects: AtomicUsize,
    writes: AtomicUsize,
    reads: AtomicUsize,
    polls: AtomicUsize,
    hang_connect: AtomicBool,
    hang_disconnect: AtomicBool,
    hang_write: AtomicBool,
    hang_poll: AtomicBool,
    hang_read: AtomicBool,
    large_sample: AtomicBool,
}
struct Mock {
    device: Arc<Device>,
    values: Values,
}
impl Backend for Mock {
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.device.connects.fetch_add(1, SeqCst);
            if self.device.hang_connect.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.device.disconnects.fetch_add(1, SeqCst);
            if self.device.hang_disconnect.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
    fn read(&mut self, member: String, _: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            self.device.reads.fetch_add(1, SeqCst);
            if self.device.hang_read.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(match member.as_str() {
                "canwrite" => json!(true),
                "minswitchvalue" => json!(0),
                _ => json!(1),
            })
        })
    }
    fn write(&mut self, _: String, _: Values) -> BackendFuture<'_, Value> {
        Box::pin(async {
            self.device.writes.fetch_add(1, SeqCst);
            if self.device.hang_write.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async {
            self.device.polls.fetch_add(1, SeqCst);
            if self.device.hang_poll.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            let mut values = self.values.clone();
            if self.device.large_sample.load(SeqCst) {
                values.insert("fixturetext".into(), json!("\"".repeat(600_000)));
            }
            Ok(values)
        })
    }
    fn reset(&mut self) {}
}

#[path = "support/runtime_host.rs"]
mod host;
#[path = "support/runtime_ipc.rs"]
mod ipc;
#[path = "support/runtime_service.rs"]
mod service;

struct Fixture {
    config: HubConfig,
    registry: Arc<SourceRegistry>,
    runtime: Arc<HubRuntime>,
    devices: Vec<Arc<Device>>,
    switch: Uuid,
    safety: Uuid,
    weather: Uuid,
}
fn fixture() -> Fixture {
    fixture_with(|_| {})
}
fn fixture_with(change: impl FnOnce(&mut HubConfig)) -> Fixture {
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/mixed-switch.json")).unwrap();
    let switch = config.outputs[0].id;
    let mut safety_config: HubConfig =
        serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap();
    config.sources.push(safety_config.sources.remove(0));
    let mut safety = safety_config.outputs.remove(0);
    if let VirtualDevice::Safety { members } = &mut safety.device {
        members.truncate(1);
        members[0].policy.safe_readings_to_safe = 1;
        members[0].policy.return_to_safe_hold_seconds = 0.0;
        members[0].policy.confirmation_seconds = 1.0;
        members[0].policy.maximum_safe_age_seconds = 3.0;
    }
    let safety_id = safety.id;
    config.outputs.push(safety);
    let weather_id = Uuid::new_v4();
    config.outputs.push(OutputConfig {
        id: weather_id,
        number: 7,
        label: "Fixture shared weather".into(),
        device: VirtualDevice::Weather {
            measurements: BTreeMap::from([(
                WeatherMetric::Temperature,
                Measurement {
                    sources: vec![Readout::Property {
                        source: config.sources[1].id,
                        property: "temperature".into(),
                        unit: None,
                    }],
                    maximum_age_seconds: 60.0,
                    average_seconds: 0.0,
                },
            )]),
        },
    });
    for source in &mut config.sources {
        source.polling.poll_seconds = 1.0;
    }
    change(&mut config);
    let devices: Vec<_> = config
        .sources
        .iter()
        .map(|_| Arc::new(Device::default()))
        .collect();
    let clock = Arc::new(MonotonicClock::default());
    let registry = Arc::new(
        SourceRegistry::build(&config, clock.clone(), |source| {
            let index = config
                .sources
                .iter()
                .position(|s| s.id == source.id)
                .unwrap();
            Ok(Box::new(Mock {
                device: devices[index].clone(),
                values: Values::from([match index {
                    0 => ("channel/0".into(), json!(1)),
                    1 => ("temperature".into(), json!(20)),
                    _ => ("issafe".into(), json!(true)),
                }]),
            }))
        })
        .unwrap(),
    );
    let runtime = HubRuntime::from_registry(config.clone(), registry.clone(), clock).unwrap();
    Fixture {
        config,
        registry,
        runtime,
        devices,
        switch,
        safety: safety_id,
        weather: weather_id,
    }
}
async fn settle() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

#[path = "support/runtime_diagnostics.rs"]
mod diagnostics;

#[tokio::test(start_paused = true)]
async fn asynchronous_connection_admission_is_bounded_and_failures_remain_visible_until_reconciled()
{
    let f = fixture();
    let client = f.runtime.client();
    f.devices[0].hang_connect.store(true, SeqCst);
    client
        .change_connection(f.switch, true, true)
        .await
        .unwrap();
    assert!(client.connecting(f.switch).unwrap());
    assert!(!client.connecting(f.weather).unwrap());
    assert!(f.runtime.active_connections() > 0);
    assert_eq!(
        client
            .change_connection(f.switch, true, true)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    assert_eq!(
        client.connect(f.weather).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    settle().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    let error = client.connecting(f.switch).unwrap_err();
    assert_ne!(error.kind, ErrorKind::Connecting);
    assert_eq!(client.connecting(f.switch).unwrap_err().kind, error.kind);
    assert!(client.connection(f.switch).is_err());
    assert_eq!(f.runtime.active_connections(), 0);
    f.devices[0].hang_connect.store(false, SeqCst);
    client
        .change_connection(f.switch, true, false)
        .await
        .unwrap();
    assert!(!client.connecting(f.switch).unwrap());
    assert!(client.connection(f.switch).is_ok());
    client
        .change_connection(f.weather, true, false)
        .await
        .unwrap();
    client
        .change_connection(f.switch, false, true)
        .await
        .unwrap();
    assert!(client.connecting(f.switch).unwrap());
    settle().await;
    assert!(!client.connecting(f.switch).unwrap());
    assert!(client.connection(f.switch).is_err());
    assert!(client.connection(f.weather).is_ok());
    client.close();
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn eof_before_an_accepted_connection_task_runs_cannot_start_source_io() {
    let f = fixture();
    let client = f.runtime.client();
    client
        .change_connection(f.switch, true, true)
        .await
        .unwrap();
    client.close();
    settle().await;
    assert_eq!(f.devices[0].connects.load(SeqCst), 0);
    assert_eq!(f.runtime.active_connections(), 0);
    assert_eq!(
        client.connecting(f.switch).unwrap_err().kind,
        ErrorKind::Disconnected
    );
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn safety_clients_share_policy_and_last_disconnect_discards_permission() {
    let f = fixture();
    settle().await;
    assert!(f.devices.iter().all(|d| d.connects.load(SeqCst) == 0));
    let a = f.runtime.client();
    let b = f.runtime.client();
    assert_ne!(a.id(), b.id());
    a.connect(f.safety).await.unwrap();
    settle().await;
    assert!(
        a.connection(f.safety)
            .unwrap()
            .safety()
            .unwrap()
            .snapshot()
            .is_safe
    );
    assert_eq!(
        b.connection(f.safety).err().unwrap().kind,
        ErrorKind::Disconnected
    );
    b.connect(f.safety).await.unwrap();
    b.connect(f.safety).await.unwrap(); // idempotent, no additional lease/policy
    let first = a.connection(f.safety).unwrap();
    let second = b.connection(f.safety).unwrap();
    assert!(std::ptr::eq(
        first.safety().unwrap(),
        second.safety().unwrap()
    ));
    let subscriber = second.safety().unwrap().subscribe();
    assert_eq!(
        f.registry
            .get(f.config.sources[2].id)
            .unwrap()
            .snapshot()
            .lease_count,
        1
    );
    assert_eq!(f.devices[2].connects.load(SeqCst), 1);
    drop(first);
    drop(second);
    a.close();
    assert_eq!(f.runtime.active_connections(), 1);
    assert!(subscriber.borrow().is_safe);
    b.close();
    assert!(!subscriber.borrow().is_safe);
    assert_eq!(f.runtime.active_connections(), 0);
    settle().await;
    assert_eq!(
        f.registry
            .get(f.config.sources[2].id)
            .unwrap()
            .snapshot()
            .lease_count,
        0
    );
    let c = f.runtime.client();
    c.connect(f.safety).await.unwrap();
    assert!(
        !c.connection(f.safety)
            .unwrap()
            .safety()
            .unwrap()
            .snapshot()
            .is_safe
    );
    settle().await;
    assert!(
        c.connection(f.safety)
            .unwrap()
            .safety()
            .unwrap()
            .snapshot()
            .is_safe
    );
}

#[tokio::test(start_paused = true)]
async fn switch_and_weather_share_source_and_weather_settings_across_clients() {
    let f = fixture();
    let a = f.runtime.client();
    let b = f.runtime.client();
    a.connect(f.weather).await.unwrap();
    b.connect(f.weather).await.unwrap();
    a.connect(f.switch).await.unwrap();
    settle().await;
    let weather_a = a.connection(f.weather).unwrap();
    let weather_b = b.connection(f.weather).unwrap();
    weather_a
        .weather()
        .unwrap()
        .set_average_period_hours(0.01)
        .unwrap();
    assert_eq!(weather_b.weather().unwrap().average_period_hours(), 0.01);
    assert_eq!(
        weather_a.safety().err().unwrap().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(f.devices[1].connects.load(SeqCst), 1);
    assert_eq!(
        f.registry
            .get(f.config.sources[1].id)
            .unwrap()
            .snapshot()
            .lease_count,
        3
    );
    assert_eq!(
        a.connection(f.switch)
            .unwrap()
            .switch()
            .unwrap()
            .value(1)
            .unwrap(),
        20.0
    );
    assert_eq!(
        weather_b
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .unwrap()
            .value,
        20.0
    );
    let descriptor = f
        .runtime
        .outputs()
        .into_iter()
        .find(|o| o.id == f.weather)
        .unwrap();
    assert_eq!(descriptor.number, 7);
    assert_eq!(f.runtime.instance_id(), f.config.instance_id);
    assert_eq!(f.runtime.revision(), f.config.revision);
    drop(weather_a);
    a.close();
    settle().await;
    assert_eq!(f.devices[1].disconnects.load(SeqCst), 0);
    assert_eq!(
        f.registry
            .get(f.config.sources[1].id)
            .unwrap()
            .snapshot()
            .lease_count,
        1
    );
    assert_eq!(
        weather_b
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .unwrap()
            .value,
        20.0
    );
    drop(weather_b);
    drop(b);
    settle().await;
    assert_eq!(f.runtime.active_connections(), 0);
    assert_eq!(f.devices[1].disconnects.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn cancelled_pending_connection_cannot_remove_its_replacement() {
    let f = fixture();
    f.devices[0].hang_connect.store(true, SeqCst);
    let client = f.runtime.client();
    let pending = tokio::spawn({
        let client = client.clone();
        let id = f.switch;
        async move { client.connect(id).await }
    });
    settle().await;
    assert_eq!(
        client.connection(f.switch).err().unwrap().kind,
        ErrorKind::Connecting
    );
    assert_eq!(
        client.connect(f.switch).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(f.runtime.active_connections(), 1);
    client.disconnect(f.switch);
    let replacement = tokio::spawn({
        let client = client.clone();
        let id = f.switch;
        async move { client.connect(id).await }
    });
    assert_eq!(
        pending.await.unwrap().unwrap_err().kind,
        ErrorKind::Disconnected
    );
    f.devices[0].hang_connect.store(false, SeqCst);
    tokio::time::advance(Duration::from_secs(2)).await;
    replacement.await.unwrap().unwrap();
    settle().await;
    assert_eq!(f.runtime.active_connections(), 1);
    assert_eq!(
        f.registry
            .get(f.config.sources[0].id)
            .unwrap()
            .snapshot()
            .lease_count,
        1
    );
    assert!(client.connection(f.switch).is_ok());
    client.close();
    assert_eq!(
        client.connect(f.switch).await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    settle().await;
    assert_eq!(f.runtime.active_connections(), 0);
}

#[tokio::test(start_paused = true)]
async fn eof_and_task_cancellation_release_pending_connections_without_blocking_safety() {
    let f = fixture();
    let client = f.runtime.client();
    client.connect(f.safety).await.unwrap();
    settle().await;
    f.devices[0].hang_connect.store(true, SeqCst);
    f.devices[2].hang_poll.store(true, SeqCst);
    let task = tokio::spawn({
        let client = client.clone();
        let id = f.switch;
        async move { client.connect(id).await }
    });
    settle().await;
    // Connection construction holds no shared lock across the stalled source.
    let safety = client.connection(f.safety).unwrap();
    assert!(safety.safety().unwrap().snapshot().is_safe);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(
        client.connection(f.switch).err().unwrap().kind,
        ErrorKind::Disconnected
    );
    let other = f.runtime.client();
    let pending = tokio::spawn({
        let other = other.clone();
        let id = f.switch;
        async move { other.connect(id).await }
    });
    settle().await;
    other.close();
    assert_eq!(
        pending.await.unwrap().unwrap_err().kind,
        ErrorKind::Disconnected
    );
    tokio::time::advance(Duration::from_secs(4)).await;
    settle().await;
    assert!(!safety.safety().unwrap().snapshot().is_safe);
    assert_eq!(
        f.registry
            .get(f.config.sources[0].id)
            .unwrap()
            .snapshot()
            .lease_count,
        0
    );
    drop(safety);
    client.close();
    assert_eq!(f.runtime.active_connections(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_inflight_write_retains_its_connection_until_its_uncertain_outcome() {
    let f = fixture();
    let client = f.runtime.client();
    client.connect(f.switch).await.unwrap();
    settle().await;
    f.devices[0].hang_write.store(true, SeqCst);
    let connection = client.connection(f.switch).unwrap();
    let task = tokio::spawn(async move { connection.switch().unwrap().set_value(0, 0.0).await });
    settle().await;
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    client.close();
    assert_eq!(f.runtime.active_connections(), 1);
    assert_eq!(
        client.connection(f.switch).err().unwrap().kind,
        ErrorKind::Disconnected
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(task.await.unwrap().unwrap_err().kind, ErrorKind::Uncertain);
    settle().await;
    assert_eq!(f.runtime.active_connections(), 0);
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    assert!(
        f.runtime
            .source_snapshots()
            .iter()
            .all(|s| s.lease_count == 0)
    );
}

#[tokio::test]
async fn stale_registries_and_unimplemented_outputs_are_rejected_before_connect() {
    let f = fixture();
    let client = f.runtime.client();
    assert_eq!(
        client.connect(Uuid::new_v4()).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    let mut config = f.config.clone();
    config.revision = Uuid::new_v4();
    let result = HubRuntime::from_registry(
        config,
        f.registry.clone(),
        Arc::new(MonotonicClock::default()),
    );
    assert_eq!(result.err().unwrap()[0].code, "revision");
    let mut config = f.config.clone();
    config.outputs[0].device = VirtualDevice::Proxy {
        source: config.sources[0].id,
        device_type: regain_hub::config::DeviceType::Switch,
    };
    let result = HubRuntime::from_registry(
        config,
        f.registry.clone(),
        Arc::new(MonotonicClock::default()),
    );
    assert!(
        result
            .err()
            .unwrap()
            .iter()
            .any(|e| e.code == "unsupported")
    );
    assert!(f.devices.iter().all(|d| d.connects.load(SeqCst) == 0));
}

#[tokio::test(start_paused = true)]
async fn shutdown_revokes_safety_before_draining_and_cannot_admit_new_clients() {
    let f = fixture();
    let client = f.runtime.client();
    client.connect(f.safety).await.unwrap();
    client.connect(f.switch).await.unwrap();
    settle().await;
    let safety = client.connection(f.safety).unwrap();
    let switches = client.connection(f.switch).unwrap();
    assert!(safety.safety().unwrap().snapshot().is_safe);
    f.devices[0].hang_disconnect.store(true, SeqCst);
    let shutdown = tokio::spawn({
        let runtime = f.runtime.clone();
        async move { runtime.shutdown().await }
    });
    settle().await;
    assert!(!safety.safety().unwrap().snapshot().is_safe);
    assert_eq!(
        client.connection(f.switch).err().unwrap().kind,
        ErrorKind::Disconnected
    );
    assert_eq!(
        f.runtime.client().connect(f.safety).await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    assert!(!shutdown.is_finished());
    assert_eq!(f.devices[0].disconnects.load(SeqCst), 1);
    tokio::time::advance(Duration::from_secs(2)).await;
    let errors = shutdown.await.unwrap().unwrap_err();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, f.config.sources[0].id);
    assert_eq!(errors[0].1.kind, ErrorKind::Uncertain);
    assert_eq!(
        switches.switch().unwrap().value(0).unwrap_err().kind,
        ErrorKind::Disconnected
    );
    assert!(
        f.runtime
            .source_snapshots()
            .iter()
            .all(|s| !s.transport_connected && s.lease_count == 0)
    );
    let again = f.runtime.shutdown().await.unwrap_err();
    assert_eq!(again[0].1.kind, ErrorKind::Uncertain);
    assert_eq!(f.devices[0].disconnects.load(SeqCst), 1);
    drop(safety);
    drop(switches);
    assert_eq!(f.runtime.active_connections(), 0);
}

#[tokio::test(start_paused = true)]
async fn cancelled_shutdown_resumes_drain_without_repeating_disconnect() {
    let f = fixture();
    let client = f.runtime.client();
    client.connect(f.switch).await.unwrap();
    let switches = client.connection(f.switch).unwrap();
    f.devices[0].hang_disconnect.store(true, SeqCst);
    let first = tokio::spawn({
        let runtime = f.runtime.clone();
        async move { runtime.shutdown().await }
    });
    settle().await;
    assert_eq!(f.devices[0].disconnects.load(SeqCst), 1);
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    let second = tokio::spawn({
        let runtime = f.runtime.clone();
        async move { runtime.shutdown().await }
    });
    settle().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        second.await.unwrap().unwrap_err()[0].1.kind,
        ErrorKind::Uncertain
    );
    assert_eq!(f.devices[0].disconnects.load(SeqCst), 1);
    drop(switches);
    assert_eq!(f.runtime.active_connections(), 0);
}
