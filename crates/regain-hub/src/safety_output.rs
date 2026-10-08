//! Safety output binds shared source events to independent membership policies.
//! It owns leases, never the source transport, and serves cached engine state.
use crate::{
    config::SafetyMember,
    parameters::FieldError,
    safety::{Clock, Endpoint, Fence, HubSnapshot, Observation, Outcome, SafetyHub, SafetyRuntime},
    source::{ErrorKind, SourceHandle, SourceRegistry, SourceSnapshot},
};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

pub struct SafetyOutput {
    runtime: Arc<SafetyRuntime>,
    stop: watch::Sender<bool>,
}
impl SafetyOutput {
    /// Construct from a validated configuration. Each enabled source contributes
    /// one required endpoint; construction cannot seed permission from a cache.
    pub fn new(
        members: &[SafetyMember],
        registry: &SourceRegistry,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, Vec<FieldError>> {
        let mut endpoints = BTreeMap::new();
        let mut inputs = Vec::new();
        for member in members.iter().filter(|member| member.enabled) {
            let source = registry
                .get(member.source)
                .map_err(|error| vec![FieldError::new("members", "source", error.message)])?;
            let state = source.snapshot();
            let endpoint = Endpoint::new(member.policy.clone(), fence(&state))?;
            if endpoints.insert(member.source, endpoint).is_some() {
                return Err(vec![FieldError::new(
                    "members",
                    "duplicate",
                    "A safety source cannot be repeated",
                )]);
            }
            // Subscribe before starting acquisition so a fast first poll cannot
            // fall between establishing the lease and listening for its result.
            let events = source.subscribe();
            let status = source.status();
            inputs.push((
                source,
                events,
                status,
                (fence(&state), state.sequence, clock.now()),
            ));
        }
        let runtime = Arc::new(SafetyRuntime::new(SafetyHub::new(endpoints), clock));
        let (stop, _) = watch::channel(false);
        for (source, events, status, initial) in inputs {
            tokio::spawn(consume(
                source,
                events,
                status,
                runtime.clone(),
                stop.subscribe(),
                initial,
            ));
        }
        Ok(Self { runtime, stop })
    }
    pub fn snapshot(&self) -> HubSnapshot {
        self.runtime.snapshot()
    }
    pub fn subscribe(&self) -> watch::Receiver<HubSnapshot> {
        self.runtime.subscribe()
    }
    /// Revoke permission synchronously before waiting for source cleanup.
    pub fn shutdown(&self) {
        self.runtime.shutdown();
        self.stop.send_replace(true);
    }
}
impl Drop for SafetyOutput {
    fn drop(&mut self) {
        // The lease cleanup tasks may outlive this object. Empty the endpoint
        // map first; even a concurrently finishing observation then stays unsafe.
        self.shutdown();
    }
}
fn fence(state: &SourceSnapshot) -> Fence {
    Fence {
        revision: state.revision,
        generation: state.generation,
    }
}

async fn consume(
    source: Arc<SourceHandle>,
    mut events: broadcast::Receiver<crate::source::PollEvent>,
    mut status: watch::Receiver<SourceSnapshot>,
    runtime: Arc<SafetyRuntime>,
    mut stop: watch::Receiver<bool>,
    initial: (Fence, u64, std::time::Duration),
) {
    let lease = Uuid::new_v4();
    let id = source.snapshot().source;
    // Start at the policy's construction epoch. Another client may connect
    // before this task is polled; the loop must observe that transition too.
    let (mut current, mut watermark, mut not_before) = initial;
    // Always finish acquisition before releasing, including when output drop
    // races a stalled connect. The actor supplies a bounded connection deadline.
    if source.acquire(lease).await.is_err() {
        return;
    }
    loop {
        if *stop.borrow() {
            break;
        }
        status.borrow_and_update();
        let latest = source.snapshot();
        if current != fence(&latest) {
            current = fence(&latest);
            watermark = 0;
            not_before = runtime.now();
            if runtime.reset_source(id, current).is_err() {
                break;
            }
        }
        tokio::select! {
            biased;
            _ = stop.changed() => break,
            changed = status.changed() => { if changed.is_err() { break; } },
            event = events.recv() => match event {
                Err(broadcast::error::RecvError::Closed) => break,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Lost events may include unsafe. Flush the retained tail,
                    // invalidate permission, and require brand new observations.
                    events = source.subscribe();
                    let latest = source.snapshot();
                    current = fence(&latest);
                    watermark = latest.sequence;
                    not_before = runtime.now();
                    if runtime.reset_source(id, current).is_err() { break; }
                }
                Ok(event) => {
                    // A status change and a sample can arrive in the same tick.
                    // Authoritative source status wins over queued old epochs.
                    let latest = source.snapshot();
                    let event_fence = Fence { revision: event.revision, generation: event.generation };
                    if event_fence != fence(&latest) { continue; }
                    if current != event_fence {
                        current = event_fence;
                        watermark = 0;
                        not_before = runtime.now();
                        if runtime.reset_source(id, current).is_err() { break; }
                    }
                    if event.sequence <= watermark { continue; }
                    watermark = event.sequence;
                    let outcome = match event.result {
                        Ok(values) => match values.get("issafe").and_then(|value| value.as_bool()) {
                            Some(true) => Outcome::Safe, Some(false) => Outcome::Unsafe, None => Outcome::Fault,
                        },
                        Err(error) if matches!(error.kind, ErrorKind::Transient | ErrorKind::Disconnected) => {
                            if event.cycle_exhausted { Outcome::CycleFailed } else { Outcome::AttemptFailed }
                        }
                        Err(_) => Outcome::Fault,
                    };
                    // A local virtual poll may expose still-valid evidence from
                    // before this output existed. It cannot seed a new policy.
                    if outcome == Outcome::Safe && event.started < not_before { continue; }
                    runtime.observe(id, Observation { resume_epoch: latest.resume_epoch, fence: current, sequence: event.sequence,
                        request_started: event.started, received: event.received, outcome });
                }
            }
        }
    }
    // EOF/shutdown cannot leave another output depending on a cached safe value.
    let _ = runtime.reset_source(id, current);
    let _ = source.release(lease).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        parameters::{PollPolicy, SafetyPolicy},
        safety::MonotonicClock,
        source::{Backend, BackendFuture, PollEvent, Values},
    };
    use serde_json::{Value, json};
    use std::time::Duration;
    struct SafeBackend;
    impl Backend for SafeBackend {
        fn connect(&mut self) -> BackendFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn disconnect(&mut self) -> BackendFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn read(&mut self, _: String, _: Values) -> BackendFuture<'_, Value> {
            Box::pin(async { Ok(json!(true)) })
        }
        fn write(&mut self, _: String, _: Values) -> BackendFuture<'_, Value> {
            Box::pin(async { Ok(Value::Null) })
        }
        fn poll(&mut self) -> BackendFuture<'_, Values> {
            Box::pin(async { Ok(Values::from([("issafe".into(), json!(true))])) })
        }
        fn reset(&mut self) {}
    }
    async fn settle() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn consumer_started_after_another_client_connects_synchronizes_its_policy_fence() {
        let clock = Arc::new(MonotonicClock::default());
        let id = Uuid::new_v4();
        let source = SourceHandle::spawn(
            id,
            Uuid::new_v4(),
            PollPolicy::default(),
            Box::new(SafeBackend),
            clock.clone(),
        )
        .unwrap();
        let original = source.snapshot();
        let policy = SafetyPolicy {
            safe_readings_to_safe: 1,
            return_to_safe_hold_seconds: 0.0,
            ..Default::default()
        };
        let runtime = Arc::new(SafetyRuntime::new(
            SafetyHub::new(BTreeMap::from([(
                id,
                Endpoint::new(policy, fence(&original)).unwrap(),
            )])),
            clock,
        ));
        let other = Uuid::new_v4();
        source.acquire(other).await.unwrap();
        settle().await;
        assert_ne!(original.generation, source.snapshot().generation);
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(consume(
            source.clone(),
            source.subscribe(),
            source.status(),
            runtime.clone(),
            stopped,
            (fence(&original), original.sequence, Duration::ZERO),
        ));
        settle().await;
        assert!(
            !runtime.snapshot().is_safe,
            "The old cached safe value cannot seed the new consumer"
        );
        tokio::time::advance(Duration::from_secs(30)).await;
        settle().await;
        assert!(
            runtime.snapshot().is_safe,
            "A new observation must be accepted after synchronizing generations"
        );
        stop.send_replace(true);
        task.await.unwrap();
        source.release(other).await.unwrap();
        source.shutdown().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_lagging_consumer_discards_safe_tail_after_losing_unsafe_event() {
        let clock = Arc::new(MonotonicClock::default());
        let id = Uuid::new_v4();
        let source = SourceHandle::spawn(
            id,
            Uuid::new_v4(),
            PollPolicy::default(),
            Box::new(SafeBackend),
            clock.clone(),
        )
        .unwrap();
        source.acquire(Uuid::new_v4()).await.unwrap();
        settle().await;
        let state = source.snapshot();
        let policy = SafetyPolicy {
            safe_readings_to_safe: 1,
            return_to_safe_hold_seconds: 0.0,
            ..SafetyPolicy::default()
        };
        let runtime = Arc::new(SafetyRuntime::new(
            SafetyHub::new(BTreeMap::from([(
                id,
                Endpoint::new(policy, fence(&state)).unwrap(),
            )])),
            clock,
        ));
        runtime.observe(
            id,
            Observation {
                resume_epoch: 0,
                fence: fence(&state),
                sequence: 1,
                request_started: Duration::ZERO,
                received: Duration::ZERO,
                outcome: Outcome::Safe,
            },
        );
        assert!(runtime.snapshot().is_safe);
        // Three source events overflow a two-event receiver. A consumer that
        // ignores Lagged would see only the final safe values and miss unsafe.
        let (sender, receiver) = broadcast::channel(2);
        for (sequence, safe) in [(2, false), (3, true), (4, true)] {
            sender
                .send(PollEvent {
                    source: id,
                    revision: state.revision,
                    generation: state.generation,
                    sequence,
                    started: Duration::ZERO,
                    received: Duration::ZERO,
                    result: Ok(Values::from([("issafe".into(), json!(safe))])),
                    cycle_exhausted: true,
                })
                .unwrap();
        }
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(consume(
            source.clone(),
            receiver,
            source.status(),
            runtime.clone(),
            stopped,
            (fence(&state), state.sequence, Duration::ZERO),
        ));
        settle().await;
        assert!(
            !runtime.snapshot().is_safe,
            "cached safe tail cannot restore permission after event loss"
        );
        tokio::time::advance(Duration::from_secs(30)).await;
        settle().await;
        assert!(
            runtime.snapshot().is_safe,
            "new source observation can recover normally"
        );
        stop.send_replace(true);
        task.await.unwrap();
        assert!(!runtime.snapshot().is_safe);
    }
}
