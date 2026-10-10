//! Actor tests use virtual time and injected I/O faults, never physical devices.
use regain_hub::{
    parameters::PollPolicy,
    safety::MonotonicClock,
    source::{
        Backend, BackendFuture, ErrorKind, SampleBatch, SourceError, SourceHandle, SourceRegistry,
        Values,
    },
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
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
    disconnects: AtomicUsize,
    polls: AtomicUsize,
    reads: AtomicUsize,
    writes: AtomicUsize,
    resets: AtomicUsize,
    offline: AtomicBool,
    hang_connect: AtomicBool,
    pending_connect: AtomicBool,
    hang_poll: AtomicBool,
    hang_read: AtomicBool,
    hang_write: AtomicBool,
    outcomes: Mutex<VecDeque<Result<Values, SourceError>>>,
    batches: Mutex<VecDeque<SampleBatch>>,
    connect_retry_after: Mutex<Option<Duration>>,
}
struct Mock(Arc<Device>);
impl Backend for Mock {
    fn connect_step(&mut self) -> BackendFuture<'_, bool> {
        Box::pin(async {
            if self.0.pending_connect.load(SeqCst) {
                Ok(false)
            } else {
                self.connect().await.map(|()| true)
            }
        })
    }
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.0.connects.fetch_add(1, SeqCst);
            if self.0.hang_connect.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.0.offline.load(SeqCst) {
                Err(SourceError {
                    retry_after: *self.0.connect_retry_after.lock().unwrap(),
                    ..SourceError::transient()
                })
            } else {
                Ok(())
            }
        })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async {
            self.0.disconnects.fetch_add(1, SeqCst);
            Ok(())
        })
    }
    fn read(&mut self, _: String, _: Values) -> BackendFuture<'_, Value> {
        Box::pin(async {
            self.0.reads.fetch_add(1, SeqCst);
            if self.0.hang_read.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(json!(42))
        })
    }
    fn write(&mut self, _: String, _: Values) -> BackendFuture<'_, Value> {
        Box::pin(async {
            self.0.writes.fetch_add(1, SeqCst);
            if self.0.hang_write.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async {
            self.0.polls.fetch_add(1, SeqCst);
            if self.0.hang_poll.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            self.0
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(Values::from([("issafe".into(), json!(true))])))
        })
    }
    fn reset(&mut self) {
        self.0.resets.fetch_add(1, SeqCst);
    }
    fn sample(&mut self) -> BackendFuture<'_, SampleBatch> {
        Box::pin(async {
            let batch = self.0.batches.lock().unwrap().pop_front();
            match batch {
                Some(batch) => Ok(batch),
                None => self.poll().await.map(SampleBatch::from),
            }
        })
    }
}
fn spawn(device: &Arc<Device>) -> Arc<SourceHandle> {
    SourceHandle::spawn(
        Uuid::new_v4(),
        Uuid::new_v4(),
        PollPolicy::default(),
        Box::new(Mock(device.clone())),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap()
}
async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn last_lease_cleanup_is_not_repeated_at_shutdown_but_a_new_connection_is_cleaned_up() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    let first = Uuid::new_v4();
    source.acquire(first).await.unwrap();
    source.release(first).await.unwrap();
    assert_eq!(device.disconnects.load(SeqCst), 1);
    let second = Uuid::new_v4();
    source.acquire(second).await.unwrap();
    assert_eq!(device.connects.load(SeqCst), 2);
    source.release(second).await.unwrap();
    assert_eq!(device.disconnects.load(SeqCst), 2);
    source.shutdown().await.unwrap();
    assert_eq!(device.disconnects.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn pending_handshake_is_bounded_even_if_an_adapter_never_finishes() {
    let device = Arc::new(Device::default());
    device.pending_connect.store(true, SeqCst);
    let source = spawn(&device);
    let mut events = source.subscribe();
    let pending = source.acquire(Uuid::new_v4()).await.unwrap();
    assert_eq!(pending.error.unwrap().kind, ErrorKind::Connecting);
    assert!(!pending.transport_connected);
    assert!(events.try_recv().is_err());
    tokio::time::advance(Duration::from_secs(31)).await;
    settle().await;
    let event = events.try_recv().unwrap();
    assert_eq!(event.result.unwrap_err().kind, ErrorKind::Transient);
    assert_eq!(device.resets.load(SeqCst), 1);
    assert!(!source.snapshot().transport_connected);
}

#[tokio::test(start_paused = true)]
async fn shutdown_finishes_inflight_io_and_rejects_queued_and_future_commands() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    source.control(lease, true).await.unwrap();
    device.hang_write.store(true, SeqCst);
    let writing = tokio::spawn({
        let source = source.clone();
        async move { source.write(lease, "move", Values::new()).await }
    });
    settle().await;
    assert_eq!(device.writes.load(SeqCst), 1);
    let stopping = tokio::spawn({
        let source = source.clone();
        async move { source.shutdown().await }
    });
    settle().await;
    let reading = tokio::spawn({
        let source = source.clone();
        async move { source.read(lease, "position", Values::new()).await }
    });
    settle().await;
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        writing.await.unwrap().unwrap_err().kind,
        ErrorKind::Uncertain
    );
    stopping.await.unwrap().unwrap();
    assert_eq!(
        reading.await.unwrap().unwrap_err().kind,
        ErrorKind::Disconnected
    );
    assert_eq!(device.reads.load(SeqCst), 0);
    assert_eq!(
        source.acquire(Uuid::new_v4()).await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    assert!(!source.snapshot().transport_connected);
    assert!(source.snapshot().values.is_empty());
    assert_eq!(source.snapshot().lease_count, 0);
    source.shutdown().await.unwrap();
    assert_eq!(device.disconnects.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn refresh_acknowledges_trigger_without_waiting_for_a_hung_sensor() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    settle().await;
    let before = source.snapshot();
    device.hang_poll.store(true, SeqCst);
    source.refresh(lease).await.unwrap();
    settle().await;
    assert_eq!(device.polls.load(SeqCst), 2);
    assert_eq!(source.snapshot().completed_passes, before.completed_passes);
    assert_eq!(source.snapshot().values, before.values);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(source.snapshot().error.unwrap().kind, ErrorKind::Transient);
}

#[tokio::test(start_paused = true)]
async fn partial_sample_cache_bounds_survive_incremental_updates() {
    use regain_hub::source::MAX_SAMPLE_TEXT_BYTES;
    let device = Arc::new(Device::default());
    for key in ["a", "b"] {
        device.batches.lock().unwrap().push_back(SampleBatch {
            values: Values::from([(key.into(), json!("x".repeat(MAX_SAMPLE_TEXT_BYTES / 2 + 1)))]),
            partial: true,
            more: key == "a",
            ..SampleBatch::default()
        });
    }
    let source = spawn(&device);
    source.acquire(Uuid::new_v4()).await.unwrap();
    settle().await;
    assert_eq!(source.snapshot().values.len(), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    let snapshot = source.snapshot();
    assert_eq!(snapshot.error.unwrap().kind, ErrorKind::Permanent);
    assert_eq!(snapshot.values.len(), 1);
    assert!(!snapshot.values.contains_key("b"));
}

#[tokio::test(start_paused = true)]
async fn flat_typed_arrays_keep_order_and_partial_cache_resource_bounds() {
    use regain_hub::source::{MAX_SAMPLE_ARRAY_ITEMS, MAX_SAMPLE_ARRAY_LENGTH};
    let device = Arc::new(Device::default());
    let initial = Values::from([
        ("names".into(), json!(["L", "Hα", ""])),
        ("focusoffsets".into(), json!([-12, 0, 17])),
        ("position".into(), json!(-1)),
    ]);
    device
        .batches
        .lock()
        .unwrap()
        .push_back(SampleBatch::from(initial.clone()));
    let source = spawn(&device);
    source.acquire(Uuid::new_v4()).await.unwrap();
    settle().await;
    assert_eq!(source.snapshot().values, initial);
    assert!(source.snapshot().error.is_none());
    source.shutdown().await.unwrap();

    let device = Arc::new(Device::default());
    let full: Values = (0..MAX_SAMPLE_ARRAY_ITEMS / MAX_SAMPLE_ARRAY_LENGTH)
        .map(|i| {
            (
                format!("array{i}"),
                json!(vec![true; MAX_SAMPLE_ARRAY_LENGTH]),
            )
        })
        .collect();
    device.batches.lock().unwrap().extend([
        SampleBatch {
            more: true,
            ..SampleBatch::from(full.clone())
        },
        SampleBatch {
            values: Values::from([("one-more".into(), json!([0]))]),
            partial: true,
            ..SampleBatch::default()
        },
    ]);
    let source = spawn(&device);
    source.acquire(Uuid::new_v4()).await.unwrap();
    settle().await;
    assert_eq!(source.snapshot().values, full);
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(source.snapshot().error.unwrap().kind, ErrorKind::Permanent);
    assert_eq!(source.snapshot().values, full);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn typed_array_cache_rejects_nested_null_oversized_and_aggregate_text_payloads() {
    use regain_hub::source::{MAX_SAMPLE_ARRAY_LENGTH, MAX_SAMPLE_TEXT_BYTES};
    for value in [
        json!([[1]]),
        json!([null]),
        json!([{"slot": 1}]),
        json!(vec![0; MAX_SAMPLE_ARRAY_LENGTH + 1]),
        json!([
            "x".repeat(MAX_SAMPLE_TEXT_BYTES / 2 + 1),
            "x".repeat(MAX_SAMPLE_TEXT_BYTES / 2 + 1)
        ]),
    ] {
        let device = Arc::new(Device::default());
        device
            .batches
            .lock()
            .unwrap()
            .push_back(SampleBatch::from(Values::from([("bad".into(), value)])));
        let source = spawn(&device);
        source.acquire(Uuid::new_v4()).await.unwrap();
        settle().await;
        assert_eq!(source.snapshot().error.unwrap().kind, ErrorKind::Permanent);
        assert!(source.snapshot().values.is_empty());
        source.shutdown().await.unwrap();
    }
    let device = Arc::new(Device::default());
    device.batches.lock().unwrap().extend([
        SampleBatch {
            values: Values::from([(
                "array".into(),
                json!(["x".repeat(MAX_SAMPLE_TEXT_BYTES / 2 + 1)]),
            )]),
            more: true,
            ..SampleBatch::default()
        },
        SampleBatch {
            values: Values::from([(
                "scalar".into(),
                json!("x".repeat(MAX_SAMPLE_TEXT_BYTES / 2 + 1)),
            )]),
            partial: true,
            ..SampleBatch::default()
        },
    ]);
    let source = spawn(&device);
    source.acquire(Uuid::new_v4()).await.unwrap();
    settle().await;
    assert_eq!(source.snapshot().values.len(), 1);
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(source.snapshot().error.unwrap().kind, ErrorKind::Permanent);
    assert_eq!(source.snapshot().values.len(), 1);
    assert!(!source.snapshot().values.contains_key("scalar"));
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn rotating_complete_batches_cannot_accumulate_unbounded_sequence_keys() {
    use regain_hub::source::MAX_SAMPLE_KEYS;
    let device = Arc::new(Device::default());
    device.batches.lock().unwrap().extend([
        SampleBatch {
            values: (0..MAX_SAMPLE_KEYS)
                .map(|i| (i.to_string(), json!(i)))
                .collect(),
            more: true,
            ..SampleBatch::default()
        },
        SampleBatch {
            values: Values::from([("new-key".into(), json!(1))]),
            ..SampleBatch::default()
        },
    ]);
    let source = spawn(&device);
    source.acquire(Uuid::new_v4()).await.unwrap();
    settle().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    let snapshot = source.snapshot();
    assert_eq!(snapshot.error.unwrap().kind, ErrorKind::Permanent);
    assert_eq!(snapshot.sample_sequences.len(), MAX_SAMPLE_KEYS);
}

#[tokio::test(start_paused = true)]
async fn clients_share_connection_and_only_final_release_closes_it() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    settle().await;
    assert_eq!(device.connects.load(SeqCst), 0);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    source.acquire(a).await.unwrap();
    source.acquire(b).await.unwrap();
    source.acquire(a).await.unwrap();
    assert_eq!(source.snapshot().lease_count, 2);
    assert_eq!(device.connects.load(SeqCst), 1);
    source.control(a, true).await.unwrap();
    assert_eq!(
        source.control(b, true).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        source
            .write(b, "set", Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    source.release(a).await.unwrap();
    assert_eq!(device.disconnects.load(SeqCst), 0);
    source.control(b, true).await.unwrap();
    source.write(b, "set", Values::new()).await.unwrap();
    source.release(b).await.unwrap();
    assert_eq!(device.disconnects.load(SeqCst), 1);
    assert_eq!(source.snapshot().lease_count, 0);
    assert!(source.snapshot().values.is_empty());
    assert_eq!(
        source
            .read(a, "value", Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Disconnected
    );
}

#[tokio::test(start_paused = true)]
async fn cached_reads_and_other_sources_progress_during_stalled_io() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    settle().await;
    let old_generation = source.snapshot().generation;
    assert_eq!(source.snapshot().values["issafe"], true);
    device.hang_poll.store(true, SeqCst);
    tokio::time::advance(Duration::from_secs(30)).await;
    settle().await;
    assert_eq!(device.polls.load(SeqCst), 2);
    assert_eq!(source.snapshot().values["issafe"], true);
    let other_device = Arc::new(Device::default());
    let other = spawn(&other_device);
    other.acquire(Uuid::new_v4()).await.unwrap();
    settle().await;
    assert_eq!(other.snapshot().values["issafe"], true);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    let failed = source.snapshot();
    assert!(!failed.transport_connected);
    assert!(failed.values.is_empty());
    assert_ne!(failed.generation, old_generation);
    assert_eq!(device.resets.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn offline_source_retains_lease_and_recovers_without_reconnecting_client() {
    let device = Arc::new(Device::default());
    device.offline.store(true, SeqCst);
    let source = spawn(&device);
    let snapshot = source.acquire(Uuid::new_v4()).await.unwrap();
    assert!(!snapshot.transport_connected);
    assert_eq!(snapshot.lease_count, 1);
    settle().await;
    device.offline.store(false, SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert!(source.snapshot().transport_connected);
    assert_eq!(source.snapshot().values["issafe"], true);
}

#[tokio::test(start_paused = true)]
async fn uncertain_write_is_not_replayed_by_retry_or_control_transfer() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    source.acquire(a).await.unwrap();
    source.acquire(b).await.unwrap();
    source.control(a, true).await.unwrap();
    device.hang_write.store(true, SeqCst);
    assert_eq!(
        source
            .write(a, "move", Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    device.hang_write.store(false, SeqCst);
    assert_eq!(source.read(b, "position", Values::new()).await.unwrap(), 42);
    source.release(a).await.unwrap();
    source.control(b, true).await.unwrap();
    assert_eq!(
        source
            .write(b, "move", Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    tokio::time::advance(Duration::from_secs(120)).await;
    settle().await;
    assert_eq!(device.writes.load(SeqCst), 1);
    assert_eq!(device.resets.load(SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn cycles_count_exhausted_attempts_and_retry_after_survives_new_clients() {
    let device = Arc::new(Device::default());
    let error = SourceError {
        retry_after: Some(Duration::from_secs(172800)),
        ..SourceError::transient()
    };
    device
        .outcomes
        .lock()
        .unwrap()
        .extend([Err(error.clone()), Err(error.clone()), Err(error)]);
    let source = spawn(&device);
    let mut events = source.subscribe();
    source.acquire(Uuid::new_v4()).await.unwrap();
    let first = events.recv().await.unwrap();
    assert!(!first.cycle_exhausted);
    source.acquire(Uuid::new_v4()).await.unwrap();
    settle().await;
    assert_eq!(device.polls.load(SeqCst), 1);
    tokio::time::advance(Duration::from_secs(86401)).await;
    settle().await;
    assert_eq!(
        device.polls.load(SeqCst),
        1,
        "must not truncate a server's retry delay"
    );
    tokio::time::advance(Duration::from_secs(86399)).await;
    let second = events.recv().await.unwrap();
    assert!(!second.cycle_exhausted);
    tokio::time::advance(Duration::from_secs(172800)).await;
    let third = events.recv().await.unwrap();
    assert!(third.cycle_exhausted);
    assert_eq!(third.sequence, 3);
    assert_eq!(
        first.generation, third.generation,
        "HTTP failures alone retain the transport generation"
    );
    tokio::time::advance(Duration::from_secs(172800)).await;
    assert!(events.recv().await.unwrap().result.is_ok());
}

#[tokio::test(start_paused = true)]
async fn cancelled_connection_request_releases_its_new_lease() {
    let device = Arc::new(Device::default());
    device.hang_connect.store(true, SeqCst);
    let source = spawn(&device);
    let handle = source.clone();
    let task = tokio::spawn(async move { handle.acquire(Uuid::new_v4()).await });
    settle().await;
    assert_eq!(device.connects.load(SeqCst), 1);
    task.abort();
    let _ = task.await;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(source.snapshot().lease_count, 0);
    assert_eq!(device.disconnects.load(SeqCst), 1);
    assert_eq!(device.polls.load(SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn read_timeout_closes_transport_and_queue_rejects_overload() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    settle().await;
    device.hang_read.store(true, SeqCst);
    let mut tasks = Vec::new();
    for _ in 0..18 {
        let handle = source.clone();
        tasks.push(tokio::spawn(async move {
            handle.read(lease, "value", Values::new()).await
        }));
        settle().await;
    }
    assert_eq!(
        tasks.pop().unwrap().await.unwrap().unwrap_err().kind,
        ErrorKind::Busy
    );
    for task in tasks.iter().skip(1) {
        task.abort();
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(
        tasks.remove(0).await.unwrap().unwrap_err().kind,
        ErrorKind::Transient
    );
    assert_eq!(
        device.reads.load(SeqCst),
        1,
        "cancelled queued reads must not reach hardware"
    );
    assert_eq!(device.resets.load(SeqCst), 1);
    device.hang_read.store(false, SeqCst);
    assert_eq!(
        source.read(lease, "value", Values::new()).await.unwrap(),
        42
    );
    assert_eq!(device.connects.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn dropping_last_handle_disconnects_actor_and_closes_status_stream() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    source.acquire(Uuid::new_v4()).await.unwrap();
    settle().await;
    let mut status = source.status();
    status.borrow_and_update();
    drop(source);
    settle().await;
    assert_eq!(device.disconnects.load(SeqCst), 1);
    assert_eq!(status.borrow().lease_count, 0);
    assert_eq!(
        status.borrow().error.as_ref().unwrap().kind,
        ErrorKind::Disconnected
    );
    status.borrow_and_update();
    assert!(status.changed().await.is_err());
}

#[tokio::test(start_paused = true)]
async fn initial_connection_retry_after_is_not_bypassed_by_first_poll() {
    let device = Arc::new(Device::default());
    device.offline.store(true, SeqCst);
    *device.connect_retry_after.lock().unwrap() = Some(Duration::from_secs(120));
    let source = spawn(&device);
    source.acquire(Uuid::new_v4()).await.unwrap();
    source.acquire(Uuid::new_v4()).await.unwrap();
    tokio::time::advance(Duration::from_secs(119)).await;
    settle().await;
    assert_eq!(device.connects.load(SeqCst), 1);
    device.offline.store(false, SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(device.connects.load(SeqCst), 2);
    assert_eq!(source.snapshot().values["issafe"], true);
}

#[tokio::test(start_paused = true)]
async fn registry_rejects_duplicate_sources_before_building_adapters_and_shares_handles() {
    let mut config: regain_hub::config::HubConfig =
        serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap();
    let device = Arc::new(Device::default());
    let registry = SourceRegistry::build(&config, Arc::new(MonotonicClock::default()), |_| {
        Ok(Box::new(Mock(device.clone())))
    })
    .unwrap();
    let a = registry.get(config.sources[0].id).unwrap();
    let b = registry.get(config.sources[0].id).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(registry.snapshots().len(), 2);
    assert_eq!(device.connects.load(SeqCst), 0);
    a.acquire(Uuid::new_v4()).await.unwrap();
    b.acquire(Uuid::new_v4()).await.unwrap();
    assert_eq!(device.connects.load(SeqCst), 1);
    config.sources[1].backend = config.sources[0].backend.clone();
    assert!(
        SourceRegistry::build(&config, Arc::new(MonotonicClock::default()), |_| panic!(
            "Must validate before constructing an adapter"
        ))
        .is_err()
    );
}

fn safety_members(config: &regain_hub::config::HubConfig) -> Vec<regain_hub::config::SafetyMember> {
    let regain_hub::config::VirtualDevice::Safety { members } = &config.outputs[0].device else {
        panic!("fixture must be safety");
    };
    members.clone()
}
fn safety_fixture() -> regain_hub::config::HubConfig {
    serde_json::from_str(include_str!("../examples/two-source-safety.json")).unwrap()
}

#[tokio::test(start_paused = true)]
async fn safety_outputs_share_sampling_but_keep_independent_confirmation_and_leases() {
    use regain_hub::safety_output::SafetyOutput;
    let config = safety_fixture();
    let device = Arc::new(Device::default());
    let clock = Arc::new(MonotonicClock::default());
    let registry = SourceRegistry::build(&config, clock.clone(), |_| {
        Ok(Box::new(Mock(device.clone())))
    })
    .unwrap();
    let strict = safety_members(&config);
    let mut fast = strict.clone();
    for member in &mut fast {
        member.policy.safe_readings_to_safe = 1;
        member.policy.return_to_safe_hold_seconds = 0.0;
    }
    let fast = SafetyOutput::new(&fast, &registry, clock.clone()).unwrap();
    let strict = SafetyOutput::new(&strict, &registry, clock).unwrap();
    settle().await;
    assert!(fast.snapshot().is_safe);
    assert!(!strict.snapshot().is_safe);
    assert_eq!(
        device.connects.load(SeqCst),
        2,
        "one connection per source, not per output"
    );
    for _ in 0..2 {
        tokio::time::advance(Duration::from_secs(30)).await;
        settle().await;
    }
    assert!(strict.snapshot().is_safe);
    let fast_status = fast.subscribe();
    drop(fast);
    assert!(
        !fast_status.borrow().is_safe,
        "drop must withdraw cached permission synchronously"
    );
    settle().await;
    assert_eq!(device.disconnects.load(SeqCst), 0);
    assert!(strict.snapshot().is_safe);
    drop(strict);
    settle().await;
    assert_eq!(device.disconnects.load(SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn source_retry_backoff_cannot_extend_safety_lifetime_or_seed_recovery() {
    use regain_hub::safety_output::SafetyOutput;
    let config = safety_fixture();
    let device = Arc::new(Device::default());
    let clock = Arc::new(MonotonicClock::default());
    let registry = SourceRegistry::build(&config, clock.clone(), |_| {
        Ok(Box::new(Mock(device.clone())))
    })
    .unwrap();
    let mut members = safety_members(&config);
    for member in &mut members {
        member.policy.safe_readings_to_safe = 1;
        member.policy.return_to_safe_hold_seconds = 0.0;
    }
    let output = SafetyOutput::new(&members, &registry, clock).unwrap();
    settle().await;
    assert!(output.snapshot().is_safe);
    let error = SourceError {
        retry_after: Some(Duration::from_secs(300)),
        ..SourceError::transient()
    };
    device
        .outcomes
        .lock()
        .unwrap()
        .extend([Err(error.clone()), Err(error)]);
    tokio::time::advance(Duration::from_secs(30)).await;
    settle().await;
    assert!(
        output.snapshot().is_safe,
        "transient failure may retain bounded grace"
    );
    let status = output.subscribe();
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    assert!(
        !status.borrow().is_safe,
        "expiry task must act without any getter or new poll"
    );
    assert_eq!(device.polls.load(SeqCst), 4);
    for _ in 0..10 {
        assert!(!output.snapshot().is_safe);
    }
    tokio::time::advance(Duration::from_secs(240)).await;
    settle().await;
    assert!(
        output.snapshot().is_safe,
        "fresh required observations can restore permission"
    );
}

#[tokio::test(start_paused = true)]
async fn safety_invalidates_on_transport_reset_before_another_poll_completes() {
    use regain_hub::safety_output::SafetyOutput;
    let config = safety_fixture();
    let device = Arc::new(Device::default());
    let clock = Arc::new(MonotonicClock::default());
    let registry = SourceRegistry::build(&config, clock.clone(), |_| {
        Ok(Box::new(Mock(device.clone())))
    })
    .unwrap();
    let mut members = safety_members(&config);
    for member in &mut members {
        member.policy.safe_readings_to_safe = 1;
        member.policy.return_to_safe_hold_seconds = 0.0;
    }
    let output = SafetyOutput::new(&members, &registry, clock).unwrap();
    settle().await;
    assert!(output.snapshot().is_safe);
    let source = registry.get(config.sources[0].id).unwrap();
    let observer = Uuid::new_v4();
    source.acquire(observer).await.unwrap();
    device.hang_read.store(true, SeqCst);
    assert_eq!(
        source
            .read(observer, "value", Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Transient
    );
    settle().await;
    assert!(!output.snapshot().is_safe);
    assert_eq!(
        device.polls.load(SeqCst),
        2,
        "reset must invalidate without awaiting next scheduled poll"
    );
}

#[tokio::test(start_paused = true)]
async fn fenced_read_rejects_an_old_generation_before_dispatching_the_backend() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    let old = source.snapshot().generation;
    source.release(lease).await.unwrap();
    source.acquire(lease).await.unwrap();
    assert_ne!(old, source.snapshot().generation);
    let error = source
        .read_fenced(lease, "maxswitch", Values::new(), Some(old))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert_eq!(device.reads.load(SeqCst), 0);
    source
        .read_fenced(
            lease,
            "maxswitch",
            Values::new(),
            Some(source.snapshot().generation),
        )
        .await
        .unwrap();
    assert_eq!(device.reads.load(SeqCst), 1);
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn polling_diagnostics_track_retry_after_cycle_exhaustion_and_actual_dispatch() {
    use regain_hub::source::{PollPhase, PollReason};
    let device = Arc::new(Device::default());
    let error = SourceError {
        retry_after: Some(Duration::from_secs(8)),
        ..SourceError::transient()
    };
    device
        .outcomes
        .lock()
        .unwrap()
        .extend([Err(error.clone()), Err(error)]);
    let policy = PollPolicy {
        attempts_per_cycle: 2,
        poll_seconds: 20.0,
        initial_backoff_seconds: 2.0,
        backoff_cap_seconds: 4.0,
        ..PollPolicy::default()
    };
    let source = SourceHandle::spawn(
        Uuid::new_v4(),
        Uuid::new_v4(),
        policy,
        Box::new(Mock(device.clone())),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    assert_eq!(source.snapshot().polling.phase, PollPhase::Idle);
    assert_eq!(source.snapshot().polling.next_poll_after_seconds, None);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    settle().await;
    let first = source.snapshot().polling;
    assert_eq!(first.phase, PollPhase::Waiting);
    assert_eq!(first.reason, Some(PollReason::Retry));
    assert_eq!(first.attempts_started, 1);
    assert_eq!(first.attempts_per_cycle, 2);
    assert_eq!(first.last_attempt, 1);
    assert_eq!(first.last_cycle_exhausted, Some(false));
    assert_eq!(first.backoff_failures, 1);
    assert_eq!(first.next_poll_after_seconds, Some(8.0));
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    assert_eq!(device.polls.load(SeqCst), 1);
    for _ in 0..10 {
        let cached = source.snapshot().polling;
        assert_eq!(cached.observed_seconds, first.observed_seconds);
        assert_eq!(cached.next_poll_after_seconds, Some(8.0));
    }
    tokio::time::advance(Duration::from_secs(6)).await;
    settle().await;
    let second = source.snapshot().polling;
    assert_eq!(device.polls.load(SeqCst), 2);
    assert_eq!(second.attempts_started, 0);
    assert_eq!(second.last_attempt, 2);
    assert_eq!(second.last_cycle_exhausted, Some(true));
    assert_eq!(second.backoff_failures, 2);
    assert_eq!(second.next_poll_after_seconds, Some(20.0));
    tokio::time::advance(Duration::from_secs(20)).await;
    settle().await;
    let success = source.snapshot().polling;
    assert_eq!(device.polls.load(SeqCst), 3);
    assert_eq!(success.reason, Some(PollReason::Periodic));
    assert_eq!(success.backoff_failures, 0);
    assert_eq!(success.last_attempt, 1);
    source.release(lease).await.unwrap();
    let idle = source.snapshot().polling;
    assert_eq!(idle.phase, PollPhase::Idle);
    assert_eq!(idle.reason, None);
    assert_eq!(idle.next_poll_after_seconds, None);
    assert_eq!(idle.last_attempt, 0);
    source.shutdown().await.unwrap();
    assert_eq!(source.snapshot().polling.phase, PollPhase::Stopped);
}

#[tokio::test(start_paused = true)]
async fn polling_diagnostics_show_pending_connection_and_inflight_sampling() {
    use regain_hub::source::{PollPhase, PollReason};
    let device = Arc::new(Device::default());
    device.pending_connect.store(true, SeqCst);
    let source = spawn(&device);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    let pending = source.snapshot().polling;
    assert_eq!(pending.phase, PollPhase::Waiting);
    assert_eq!(pending.reason, Some(PollReason::Connection));
    assert_eq!(pending.next_poll_after_seconds, Some(0.1));
    assert_eq!(pending.last_attempt, 0);
    device.pending_connect.store(false, SeqCst);
    device.hang_poll.store(true, SeqCst);
    tokio::time::advance(Duration::from_millis(100)).await;
    settle().await;
    let sampling = source.snapshot().polling;
    assert_eq!(sampling.phase, PollPhase::Sampling);
    assert_eq!(sampling.next_poll_after_seconds, None);
    assert_eq!(sampling.last_attempt, 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(source.snapshot().polling.phase, PollPhase::Waiting);
    assert_eq!(source.snapshot().polling.reason, Some(PollReason::Retry));
    assert_eq!(device.resets.load(SeqCst), 1);
    source.release(lease).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn polling_diagnostics_preserve_suspended_retry_and_partial_pass_identity() {
    use regain_hub::source::{PollPhase, PollReason};
    let device = Arc::new(Device::default());
    device.batches.lock().unwrap().push_back(SampleBatch {
        values: Values::from([("value".into(), json!(1))]),
        more: true,
        ..SampleBatch::default()
    });
    device.outcomes.lock().unwrap().push_back(Err(SourceError {
        retry_after: Some(Duration::MAX),
        ..SourceError::transient()
    }));
    let source = spawn(&device);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    settle().await;
    let continuation = source.snapshot().polling;
    assert_eq!(continuation.phase, PollPhase::Waiting);
    assert_eq!(continuation.reason, Some(PollReason::Continuation));
    assert_eq!(continuation.next_poll_after_seconds, Some(0.001));
    assert_eq!(continuation.last_cycle_exhausted, Some(false));
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    let suspended = source.snapshot().polling;
    assert_eq!(suspended.phase, PollPhase::Suspended);
    assert_eq!(suspended.reason, Some(PollReason::Retry));
    assert_eq!(suspended.next_poll_after_seconds, None);
    tokio::time::advance(Duration::from_secs(60)).await;
    settle().await;
    assert_eq!(device.polls.load(SeqCst), 1);
    assert_eq!(
        source.snapshot().polling.observed_seconds,
        suspended.observed_seconds
    );
    source.release(lease).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn polling_diagnostics_are_readable_during_a_stalled_initial_connection() {
    use regain_hub::source::PollPhase;
    let device = Arc::new(Device::default());
    device.hang_connect.store(true, SeqCst);
    let source = spawn(&device);
    let lease = Uuid::new_v4();
    let acquiring = tokio::spawn({
        let source = source.clone();
        async move { source.acquire(lease).await }
    });
    settle().await;
    let connecting = source.snapshot().polling;
    assert_eq!(connecting.phase, PollPhase::Connecting);
    assert_eq!(connecting.next_poll_after_seconds, None);
    assert_eq!(connecting.attempts_started, 0);
    for _ in 0..10 {
        assert_eq!(
            source.snapshot().polling.observed_seconds,
            connecting.observed_seconds
        );
    }
    assert_eq!(device.connects.load(SeqCst), 1);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert!(acquiring.await.unwrap().is_ok());
    assert_eq!(source.snapshot().polling.phase, PollPhase::Waiting);
    assert_eq!(device.resets.load(SeqCst), 1);
    source.release(lease).await.unwrap();
    source.shutdown().await.unwrap();
}
