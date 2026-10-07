use regain_hub::{
    config::{ConfigStore, HubConfig, VirtualDevice},
    readout::SourceLease,
    safety::MonotonicClock,
    source::{Backend, BackendFuture, ErrorKind, SourceRegistry, Values},
    switch::{Grid, SwitchOutput},
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
    connections: AtomicUsize,
    disconnects: AtomicUsize,
    writable: AtomicBool,
    hang_write: AtomicBool,
    hold_next_poll: AtomicBool,
    poll_held: tokio::sync::Notify,
    release_poll: tokio::sync::Notify,
    writes: Mutex<Vec<f64>>,
    step: Mutex<f64>,
    value: Mutex<f64>,
}
struct Mock(Arc<Device>);
impl Backend for Mock {
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.0.connections.fetch_add(1, SeqCst);
            Ok(())
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
            assert_eq!(parameters["Id"], 0);
            Ok(match member.as_str() {
                "canwrite" => json!(self.0.writable.load(SeqCst)),
                "minswitchvalue" => json!(0.0),
                "maxswitchvalue" => json!(10.0),
                "switchstep" => json!(*self.0.step.lock().unwrap()),
                _ => panic!("unexpected member {member}"),
            })
        })
    }
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            assert_eq!(member, "setswitchvalue");
            assert_eq!(parameters["Id"], 0);
            let value = parameters["Value"].as_f64().unwrap();
            self.0.writes.lock().unwrap().push(value);
            if self.0.hang_write.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            *self.0.value.lock().unwrap() = value;
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async {
            if self.0.hold_next_poll.swap(false, SeqCst) {
                self.0.poll_held.notify_one();
                self.0.release_poll.notified().await;
            }
            Ok(Values::from([
                ("channel/0".into(), json!(*self.0.value.lock().unwrap())),
                ("temperature".into(), json!(12.0)),
            ]))
        })
    }
    fn reset(&mut self) {}
}
fn fixture() -> HubConfig {
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/mixed-switch.json")).unwrap();
    let VirtualDevice::Switch { channels } = &mut config.outputs[0].device else {
        unreachable!()
    };
    channels[0].minimum = 2.0;
    channels[0].maximum = 8.0;
    channels[0].step = 2.0;
    config
}
fn device() -> Arc<Device> {
    Arc::new(Device {
        writable: AtomicBool::new(true),
        step: Mutex::new(2.0),
        value: Mutex::new(2.0),
        ..Device::default()
    })
}
fn setup(config: &HubConfig, device: &Arc<Device>) -> (Arc<SourceRegistry>, Arc<SwitchOutput>) {
    let clock = Arc::new(MonotonicClock::default());
    let registry = Arc::new(
        SourceRegistry::build(config, clock.clone(), |_| {
            Ok(Box::new(Mock(device.clone())))
        })
        .unwrap(),
    );
    let output = SwitchOutput::new(config, config.outputs[0].id, registry.clone(), clock).unwrap();
    (registry, output)
}
async fn settle() {
    for _ in 0..12 {
        tokio::task::yield_now().await;
    }
}

#[test]
fn grid_rounds_nearest_steps_and_rejects_invalid_bounds_before_io() {
    let grid = Grid::new(5.0, 8.0, 1.0).unwrap();
    for (input, expected) in [(5.49, 5.0), (5.5, 6.0), (7.9, 8.0)] {
        assert_eq!(grid.quantize(input).unwrap(), expected);
    }
    for value in [f64::NAN, 4.99, 8.01] {
        assert!(grid.quantize(value).is_err());
    }
    for (min, max, step) in [
        (0.0, 1.0, 0.3),
        (0.0, 0.0, 1.0),
        (0.0, 1.0, 0.0),
        (0.0, 1.0, 1e-20),
    ] {
        assert!(Grid::new(min, max, step).is_err());
    }
    assert!(Grid::new(0.0, 1.0, 0.1).is_ok());
}

#[tokio::test(start_paused = true)]
async fn acknowledged_switch_write_is_unavailable_until_fresh_poll_confirms_it_for_both_clients() {
    let config = fixture();
    let device = device();
    let (registry, output) = setup(&config, &device);
    let first = output.connect().await.unwrap();
    let second = output.connect().await.unwrap();
    settle().await;
    assert_eq!(first.value(0).unwrap(), 2.0);
    assert_eq!(second.value(0).unwrap(), 2.0);
    device.hold_next_poll.store(true, SeqCst);
    first.set_value(0, 4.0).await.unwrap();
    device.poll_held.notified().await;
    let state = registry.get(config.sources[0].id).unwrap().snapshot();
    assert!(state.transport_connected && state.error.is_none() && !state.write_uncertain);
    assert!(state.values.is_empty());
    assert_eq!(first.value(0).unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(second.value(0).unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(*device.writes.lock().unwrap(), vec![4.0]);
    device.release_poll.notify_one();
    settle().await;
    assert_eq!(first.value(0).unwrap(), 4.0);
    assert_eq!(second.value(0).unwrap(), 4.0);
    assert_eq!(*device.writes.lock().unwrap(), vec![4.0]);
}

#[tokio::test(start_paused = true)]
async fn combined_gauges_and_switches_share_sources_and_preserve_boolean_semantics() {
    let config = fixture();
    let device = device();
    let (_, output) = setup(&config, &device);
    let first = output.connect().await.unwrap();
    let second = output.connect().await.unwrap();
    settle().await;
    assert_eq!(device.connections.load(SeqCst), 2);
    assert_eq!(first.value(1).unwrap(), 12.0);
    assert!(!first.state(0).unwrap());
    assert!(!first.can_write(1).await.unwrap());
    assert!(first.can_write(0).await.unwrap());
    first.set_value(0, 3.0).await.unwrap();
    settle().await;
    assert_eq!(first.value(0).unwrap(), 4.0);
    assert!(first.state(0).unwrap());
    first.set_state(0, true).await.unwrap();
    settle().await;
    second.set_state(0, false).await.unwrap();
    settle().await;
    assert_eq!(*device.writes.lock().unwrap(), vec![4.0, 8.0, 2.0]);
    assert_eq!(
        first.set_value(1, 20.0).await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    drop(first);
    settle().await;
    assert_eq!(device.disconnects.load(SeqCst), 0);
    drop(second);
    settle().await;
    assert_eq!(device.disconnects.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn upstream_permissions_steps_and_exclusive_control_cannot_be_overridden() {
    let config = fixture();
    let device = device();
    let (registry, output) = setup(&config, &device);
    let client = output.connect().await.unwrap();
    device.writable.store(false, SeqCst);
    assert!(!client.can_write(0).await.unwrap());
    assert_eq!(
        client.set_state(0, true).await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    settle().await;
    device.writable.store(true, SeqCst);
    *device.step.lock().unwrap() = 1.0;
    client.set_value(0, 4.0).await.unwrap();
    settle().await;
    *device.step.lock().unwrap() = 2.5;
    assert_eq!(
        client.set_value(0, 4.0).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    settle().await;
    *device.step.lock().unwrap() = 2.0;
    let owner = SourceLease::acquire(registry.get(config.sources[0].id).unwrap())
        .await
        .unwrap();
    owner.source.control(owner.id, true).await.unwrap();
    assert_eq!(
        client.set_state(0, false).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(device.writes.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn cancelled_write_releases_its_operation_lease_without_replaying() {
    let config = fixture();
    let device = device();
    let (registry, output) = setup(&config, &device);
    let client = Arc::new(output.connect().await.unwrap());
    device.hang_write.store(true, SeqCst);
    let task_client = client.clone();
    let task = tokio::spawn(async move { task_client.set_state(0, true).await });
    settle().await;
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    task.abort();
    let _ = task.await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    let source = registry.get(config.sources[0].id).unwrap();
    assert_eq!(source.snapshot().lease_count, 1);
    device.hang_write.store(false, SeqCst);
    assert_eq!(
        client.set_state(0, true).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(device.writes.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn removed_channel_keeps_its_slot_after_persisted_configuration_reload() {
    let store = ConfigStore::new(None, fixture()).unwrap();
    let old = store.snapshot();
    let mut edited = old.clone();
    let VirtualDevice::Switch { channels } = &mut edited.outputs[0].device else {
        unreachable!()
    };
    channels.remove(1);
    store.apply(old.revision, edited, false).unwrap();
    let config: HubConfig =
        serde_json::from_str(&serde_json::to_string(&store.snapshot()).unwrap()).unwrap();
    let device = device();
    let (_, output) = setup(&config, &device);
    let client = output.connect().await.unwrap();
    assert_eq!(output.max_switch(), 2);
    assert!(output.name(1).unwrap().contains("Removed"));
    assert!(!client.can_write(1).await.unwrap());
    assert_eq!(client.value(1).unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(client.value(2).unwrap_err().kind, ErrorKind::InvalidValue);
}

#[tokio::test(start_paused = true)]
async fn a_write_fence_is_checked_inside_the_source_actor() {
    let config = fixture();
    let device = device();
    let (registry, _) = setup(&config, &device);
    let owner = SourceLease::acquire(registry.get(config.sources[0].id).unwrap())
        .await
        .unwrap();
    owner.source.control(owner.id, true).await.unwrap();
    let error = owner
        .source
        .write_fenced(
            owner.id,
            "setswitchvalue",
            Values::new(),
            Some(Uuid::new_v4()),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert!(device.writes.lock().unwrap().is_empty());
}
