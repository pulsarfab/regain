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
    hang_poll: AtomicBool,
    hang_read: AtomicBool,
    hang_write: AtomicBool,
    outcomes: Mutex<VecDeque<Result<Values, SourceError>>>,
    batches: Mutex<VecDeque<SampleBatch>>,
    connect_retry_after: Mutex<Option<Duration>>,
}
struct Mock(Arc<Device>);
impl Backend for Mock {
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
