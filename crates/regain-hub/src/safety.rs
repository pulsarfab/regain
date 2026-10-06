//! Field Kit-inspired safety state machine, adapted for shared source sampling.
//! Copyright 2026 Yann Ramin; Apache-2.0. This Rust implementation adds observation
//! fencing, per-membership cadence, and clears permission on configuration changes.
use crate::parameters::{FieldError, ParameterSet, SafetyPolicy};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fence {
    pub revision: Uuid,
    pub generation: Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Safe,
    Unsafe,
    /// One read attempt failed, but its cycle has not exhausted its attempts.
    AttemptFailed,
    /// All attempts in the source's read cycle failed.
    CycleFailed,
    /// Invalid data, authentication/configuration errors or a local worker fault.
    Fault,
}

#[derive(Clone, Copy, Debug)]
pub struct Observation {
    pub fence: Fence,
    pub sequence: u64,
    pub request_started: Duration,
    pub received: Duration,
    pub outcome: Outcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Unknown,
    FreshSafe,
    GraceSafe,
    PendingUnsafe,
    PendingSafe,
    Unsafe,
    Stale,
    Faulted,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub phase: Phase,
    pub raw_is_safe: Option<bool>,
    pub permits_safe: bool,
    pub recovery_confirmed: bool,
    pub safe_age_seconds: Option<f64>,
    pub failed_cycles: u32,
    pub unsafe_readings: u32,
    pub safe_readings: u32,
    pub safe_hold_seconds: f64,
    pub reason: &'static str,
    pub last_sequence: u64,
}

pub struct Endpoint {
    policy: SafetyPolicy,
    fence: Fence,
    phase: Phase,
    raw: Option<bool>,
    permits: bool,
    confirmed: bool,
    sequence: u64,
    last_now: Duration,
    last_request: Option<Duration>,
    last_safe_start: Option<Duration>,
    run_start: Option<Duration>,
    counted_safe: Option<Duration>,
    counted_unsafe: Option<Duration>,
    counted_failure: Option<Duration>,
    failures: u32,
    unsafe_count: u32,
    safe_count: u32,
    reason: &'static str,
    clock_fault: bool,
}

impl Endpoint {
    pub fn new(policy: SafetyPolicy, fence: Fence) -> Result<Self, Vec<FieldError>> {
        let errors = policy.validate();
        if !errors.is_empty() {
            return Err(errors);
        }
        if fence.revision.is_nil() || fence.generation.is_nil() {
            return Err(vec![FieldError::new(
                "fence",
                "identity",
                "Safety observations require non-nil revision and generation IDs",
            )]);
        }
        Ok(Self {
            policy,
            fence,
            phase: Phase::Unknown,
            raw: None,
            permits: false,
            confirmed: false,
            sequence: 0,
            last_now: Duration::ZERO,
            last_request: None,
            last_safe_start: None,
            run_start: None,
            counted_safe: None,
            counted_unsafe: None,
            counted_failure: None,
            failures: 0,
            unsafe_count: 0,
            safe_count: 0,
            reason: "Waiting for a valid safe observation",
            clock_fault: false,
        })
    }
    fn reset_recovery(&mut self) {
        self.safe_count = 0;
        self.run_start = None;
        self.counted_safe = None;
        self.confirmed = false;
    }
    fn withdraw(&mut self, phase: Phase, reason: &'static str) {
        self.permits = false;
        self.reset_recovery();
        self.phase = phase;
        self.reason = reason;
    }
    fn age_limit(&self) -> Duration {
        Duration::from_secs_f64(self.policy.maximum_safe_age_seconds)
    }
    fn eligible(&self, last: Option<Duration>, start: Duration) -> bool {
        last.is_none_or(|last| {
            start.saturating_sub(last) >= Duration::from_secs_f64(self.policy.confirmation_seconds)
        })
    }
    fn expire(&mut self, now: Duration) {
        if now < self.last_now {
            self.clock_fault = true;
            self.withdraw(
                Phase::Faulted,
                "Clock discontinuity; reconnect to establish a new generation",
            );
        }
        self.last_now = now;
        if self
            .last_safe_start
            .is_some_and(|start| now.saturating_sub(start) >= self.age_limit())
            && !matches!(self.phase, Phase::Stale | Phase::Unsafe | Phase::Faulted)
        {
            self.withdraw(Phase::Stale, "Last safe request exceeded its maximum age");
        }
    }
    /// Returns false for replayed/retired observations. They still cannot prevent
    /// an existing deadline from expiring. Adapters classify errors, not permission.
    pub fn observe(&mut self, observation: Observation, now: Duration) -> bool {
        self.expire(now);
        if self.clock_fault
            || observation.fence != self.fence
            || observation.sequence <= self.sequence
        {
            return false;
        }
        self.sequence = observation.sequence;
        let start = observation.request_started;
        if start > observation.received
            || observation.received > now
            || self.last_request.is_some_and(|previous| start < previous)
        {
            self.withdraw(Phase::Faulted, "Invalid observation timestamps");
            return true;
        }
        self.last_request = Some(start);
        match observation.outcome {
            Outcome::Fault => self.withdraw(
                Phase::Faulted,
                "Source returned invalid data or a permanent error",
            ),
            Outcome::AttemptFailed | Outcome::CycleFailed => {
                self.reset_recovery();
                if self.permits {
                    self.phase = if self.unsafe_count > 0 {
                        Phase::PendingUnsafe
                    } else {
                        Phase::GraceSafe
                    };
                } else if self.phase == Phase::PendingSafe {
                    self.phase = Phase::Unknown;
                }
                self.reason = if self.phase == Phase::Unsafe
                    && self.unsafe_count >= self.policy.unsafe_readings_to_unsafe
                {
                    "Confirmed upstream unsafe; communication also failed"
                } else {
                    "Source communication failed"
                };
                if observation.outcome == Outcome::CycleFailed
                    && self.eligible(self.counted_failure, start)
                {
                    self.counted_failure = Some(start);
                    self.failures = self.failures.saturating_add(1);
                    if self.failures >= self.policy.failed_cycles_to_unsafe {
                        self.withdraw(Phase::Unsafe, "Failed check threshold reached");
                    }
                }
            }
            Outcome::Unsafe => {
                self.raw = Some(false);
                self.failures = 0;
                self.counted_failure = None;
                self.reset_recovery();
                if self.eligible(self.counted_unsafe, start) {
                    self.counted_unsafe = Some(start);
                    self.unsafe_count = self.unsafe_count.saturating_add(1);
                }
                if self.unsafe_count >= self.policy.unsafe_readings_to_unsafe {
                    self.withdraw(
                        Phase::Unsafe,
                        "Upstream reports unsafe; confirmation threshold reached",
                    );
                } else {
                    self.phase = Phase::PendingUnsafe;
                    self.reason = "Upstream reports unsafe; prior safe age still applies";
                }
            }
            Outcome::Safe => {
                self.raw = Some(true);
                self.failures = 0;
                self.counted_failure = None;
                self.unsafe_count = 0;
                self.counted_unsafe = None;
                if now.saturating_sub(start) >= self.age_limit() {
                    self.withdraw(Phase::Stale, "Safe response was already expired on receipt");
                    return true;
                }
                self.last_safe_start = Some(start);
                if self.eligible(self.counted_safe, start) {
                    self.counted_safe = Some(start);
                    self.run_start.get_or_insert(observation.received);
                    self.safe_count = self.safe_count.saturating_add(1);
                    self.confirmed = self.safe_count >= self.policy.safe_readings_to_safe
                        && self.run_start.is_some_and(|t| {
                            now.saturating_sub(t)
                                >= Duration::from_secs_f64(self.policy.return_to_safe_hold_seconds)
                        });
                    if self.confirmed {
                        self.permits = true;
                    }
                }
                self.phase = if self.permits {
                    Phase::FreshSafe
                } else {
                    Phase::PendingSafe
                };
                self.reason = if self.permits {
                    "Fresh safe observation"
                } else {
                    "Waiting for safe reading count and recovery hold"
                };
            }
        }
        self.expire(now);
        true
    }
    pub fn snapshot(&mut self, now: Duration) -> Snapshot {
        self.expire(now);
        Snapshot {
            phase: self.phase,
            raw_is_safe: self.raw,
            permits_safe: self.permits,
            recovery_confirmed: self.permits && self.confirmed,
            safe_age_seconds: self
                .last_safe_start
                .map(|t| now.saturating_sub(t).as_secs_f64()),
            failed_cycles: self.failures,
            unsafe_readings: self.unsafe_count,
            safe_readings: self.safe_count,
            safe_hold_seconds: self
                .run_start
                .map_or(0.0, |t| now.saturating_sub(t).as_secs_f64()),
            reason: self.reason,
            last_sequence: self.sequence,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSnapshot {
    pub is_safe: bool,
    pub endpoints: BTreeMap<Uuid, Snapshot>,
    /// Internal evidence timestamp for local composition; never persisted.
    #[serde(skip)]
    pub(crate) safe_observed_at: Option<Duration>,
}

pub struct SafetyHub {
    endpoints: BTreeMap<Uuid, Endpoint>,
    aggregate_safe: bool,
}
impl SafetyHub {
    pub fn new(endpoints: BTreeMap<Uuid, Endpoint>) -> Self {
        Self {
            endpoints,
            aggregate_safe: false,
        }
    }
    /// Reconnect, configuration replacement, and resume invalidate all prior
    /// evidence. Late polls from the former generation are rejected afterward.
    pub fn reset_generation(&mut self, fence: Fence) -> Result<(), Vec<FieldError>> {
        let replacement: Result<BTreeMap<_, _>, _> = self
            .endpoints
            .iter()
            .map(|(id, state)| Endpoint::new(state.policy.clone(), fence).map(|state| (*id, state)))
            .collect();
        self.endpoints = replacement?;
        self.aggregate_safe = false;
        Ok(())
    }
    pub fn reset_source(&mut self, source: Uuid, fence: Fence) -> Result<(), Vec<FieldError>> {
        let Some(previous) = self.endpoints.get(&source) else {
            return Ok(());
        };
        let replacement = Endpoint::new(previous.policy.clone(), fence)?;
        self.endpoints.insert(source, replacement);
        self.aggregate_safe = false;
        Ok(())
    }
    pub fn observe(&mut self, source: Uuid, observation: Observation, now: Duration) -> bool {
        // Observe the previous aggregate before applying a result: a late safe
        // response must not hide expiry or restore an aggregate using grace.
        self.snapshot(now);
        let accepted = self
            .endpoints
            .get_mut(&source)
            .is_some_and(|s| s.observe(observation, now));
        self.snapshot(now);
        accepted
    }
    pub fn snapshot(&mut self, now: Duration) -> HubSnapshot {
        let endpoints: BTreeMap<_, _> = self
            .endpoints
            .iter_mut()
            .map(|(id, s)| (*id, s.snapshot(now)))
            .collect();
        if endpoints.is_empty() || endpoints.values().any(|s| !s.permits_safe) {
            self.aggregate_safe = false;
        } else if !self.aggregate_safe {
            self.aggregate_safe = endpoints
                .values()
                .all(|s| s.phase == Phase::FreshSafe && s.recovery_confirmed);
        }
        HubSnapshot {
            is_safe: self.aggregate_safe,
            endpoints,
            safe_observed_at: self
                .aggregate_safe
                .then(|| {
                    self.endpoints
                        .values()
                        .filter_map(|s| s.last_safe_start)
                        .min()
                })
                .flatten(),
        }
    }
}

/// A local clock can be replaced by a deterministic clock in integration tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}
pub struct MonotonicClock(tokio::time::Instant);
impl Default for MonotonicClock {
    fn default() -> Self {
        Self(tokio::time::Instant::now())
    }
}
impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// Expiry has its own task and never awaits a source or transport operation.
/// Dropping this runtime stops that task; no cached safe state is persisted.
pub struct SafetyRuntime {
    hub: Arc<Mutex<SafetyHub>>,
    clock: Arc<dyn Clock>,
    updates: watch::Sender<HubSnapshot>,
    expiry: tokio::task::JoinHandle<()>,
}
impl SafetyRuntime {
    pub(crate) fn now(&self) -> Duration {
        self.clock.now()
    }
    pub fn new(mut hub: SafetyHub, clock: Arc<dyn Clock>) -> Self {
        let initial = hub.snapshot(clock.now());
        let hub = Arc::new(Mutex::new(hub));
        let (updates, _) = watch::channel(initial);
        let (task_hub, task_clock, task_updates) = (hub.clone(), clock.clone(), updates.clone());
        let expiry = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let mut hub = task_hub.lock().unwrap();
                task_updates.send_replace(hub.snapshot(task_clock.now()));
            }
        });
        Self {
            hub,
            clock,
            updates,
            expiry,
        }
    }
    pub fn observe(&self, source: Uuid, observation: Observation) -> bool {
        let mut hub = self.hub.lock().unwrap();
        let now = self.clock.now();
        let accepted = hub.observe(source, observation, now);
        self.updates.send_replace(hub.snapshot(now));
        accepted
    }
    pub fn snapshot(&self) -> HubSnapshot {
        self.hub.lock().unwrap().snapshot(self.clock.now())
    }
    pub fn subscribe(&self) -> watch::Receiver<HubSnapshot> {
        self.updates.subscribe()
    }
    pub fn reset_generation(&self, fence: Fence) -> Result<(), Vec<FieldError>> {
        let mut hub = self.hub.lock().unwrap();
        hub.reset_generation(fence)?;
        self.updates.send_replace(hub.snapshot(self.clock.now()));
        Ok(())
    }
    pub fn reset_source(&self, source: Uuid, fence: Fence) -> Result<(), Vec<FieldError>> {
        let mut hub = self.hub.lock().unwrap();
        hub.reset_source(source, fence)?;
        self.updates.send_replace(hub.snapshot(self.clock.now()));
        Ok(())
    }
    /// Withdraw permission synchronously before asynchronous lease cleanup.
    pub fn shutdown(&self) {
        let mut hub = self.hub.lock().unwrap();
        *hub = SafetyHub::new(BTreeMap::new());
        self.updates.send_replace(hub.snapshot(self.clock.now()));
    }
}
impl Drop for SafetyRuntime {
    fn drop(&mut self) {
        self.expiry.abort();
        // Receivers may outlive their publisher. Never leave them holding a
        // final safe snapshot after shutdown or configuration replacement.
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn t(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }
    fn fence() -> Fence {
        Fence {
            revision: Uuid::new_v4(),
            generation: Uuid::new_v4(),
        }
    }
    fn policy() -> SafetyPolicy {
        SafetyPolicy {
            confirmation_seconds: 1.0,
            ..SafetyPolicy::default()
        }
    }
    fn obs(fence: Fence, sequence: u64, seconds: u64, outcome: Outcome) -> Observation {
        Observation {
            fence,
            sequence,
            request_started: t(seconds),
            received: t(seconds),
            outcome,
        }
    }
    fn ready() -> (Endpoint, Fence) {
        let f = fence();
        let mut s = Endpoint::new(policy(), f).unwrap();
        for (seq, time) in [(1, 0), (2, 5), (3, 10)] {
            s.observe(obs(f, seq, time, Outcome::Safe), t(time));
        }
        assert!(s.snapshot(t(10)).permits_safe);
        (s, f)
    }
    #[test]
    fn startup_and_empty_aggregate_are_unsafe() {
        assert!(
            !Endpoint::new(policy(), fence())
                .unwrap()
                .snapshot(t(0))
                .permits_safe
        );
        assert!(!SafetyHub::new(BTreeMap::new()).snapshot(t(0)).is_safe);
    }
    #[test]
    fn getters_cannot_finish_hold_or_count_as_observations() {
        let f = fence();
        let mut s = Endpoint::new(policy(), f).unwrap();
        for i in 0..3 {
            s.observe(obs(f, i + 1, i, Outcome::Safe), t(i));
        }
        assert!(!s.snapshot(t(10)).permits_safe);
        assert_eq!(s.snapshot(t(10)).safe_readings, 3);
        s.observe(obs(f, 4, 10, Outcome::Safe), t(10));
        assert!(s.snapshot(t(10)).permits_safe);
    }
    #[test]
    fn retries_clear_recovery_without_counting_failed_cycles() {
        let (mut s, f) = ready();
        s.observe(obs(f, 4, 11, Outcome::AttemptFailed), t(11));
        let snap = s.snapshot(t(11));
        assert!(snap.permits_safe);
        assert_eq!(snap.phase, Phase::GraceSafe);
        assert_eq!(snap.failed_cycles, 0);
        assert_eq!(snap.safe_readings, 0);
        s.observe(obs(f, 5, 12, Outcome::Safe), t(12));
        assert!(!s.snapshot(t(12)).recovery_confirmed);
    }
    #[test]
    fn expiry_during_outage_requires_new_confirmations() {
        let (mut s, f) = ready();
        s.observe(obs(f, 4, 11, Outcome::CycleFailed), t(11));
        assert!(s.snapshot(t(99)).permits_safe);
        assert!(!s.snapshot(t(100)).permits_safe);
        s.observe(obs(f, 5, 100, Outcome::Safe), t(100));
        assert!(!s.snapshot(t(100)).permits_safe);
        assert_eq!(s.snapshot(t(100)).safe_readings, 1);
    }
    #[test]
    fn explicit_unsafe_and_permanent_fault_withdraw_immediately() {
        for outcome in [Outcome::Unsafe, Outcome::Fault] {
            let (mut s, f) = ready();
            s.observe(obs(f, 4, 11, outcome), t(11));
            assert!(!s.snapshot(t(11)).permits_safe);
        }
    }
    #[test]
    fn failed_cycle_threshold_is_independent_of_age() {
        let (mut s, f) = ready();
        for seq in 4..7 {
            s.observe(obs(f, seq, seq + 10, Outcome::CycleFailed), t(seq + 10));
        }
        let snap = s.snapshot(t(16));
        assert_eq!(snap.failed_cycles, 3);
        assert_eq!(snap.phase, Phase::Unsafe);
    }
    #[test]
    fn fast_shared_polling_does_not_accelerate_safe_or_unsafe_counts() {
        let f = fence();
        let p = SafetyPolicy {
            unsafe_readings_to_unsafe: 2,
            ..SafetyPolicy::default()
        };
        let mut s = Endpoint::new(p, f).unwrap();
        for time in 0..=60 {
            s.observe(obs(f, time + 1, time, Outcome::Safe), t(time));
        }
        assert!(s.snapshot(t(60)).permits_safe);
        assert_eq!(s.snapshot(t(60)).safe_readings, 3);
        for time in 61..91 {
            s.observe(obs(f, time + 1, time, Outcome::Unsafe), t(time));
        }
        assert_eq!(s.snapshot(t(90)).unsafe_readings, 1);
        s.observe(obs(f, 92, 91, Outcome::Unsafe), t(91));
        assert!(!s.snapshot(t(91)).permits_safe);
    }
    #[test]
    fn replay_and_retired_generations_cannot_refresh_safe_evidence() {
        let (mut s, f) = ready();
        assert!(!s.observe(obs(f, 3, 90, Outcome::Safe), t(90)));
        assert!(!s.observe(obs(fence(), 100, 91, Outcome::Safe), t(91)));
        assert!(!s.snapshot(t(100)).permits_safe);
    }
    #[test]
    fn late_safe_response_cannot_hide_expiry() {
        let (mut s, f) = ready();
        let mut result = obs(f, 4, 11, Outcome::Safe);
        result.received = t(101);
        s.observe(result, t(101));
        assert_eq!(s.snapshot(t(101)).phase, Phase::Stale);
    }
    #[test]
    fn clock_reversal_and_invalid_timestamps_fail_closed() {
        let (mut s, f) = ready();
        s.snapshot(t(9));
        assert!(!s.observe(obs(f, 4, 12, Outcome::Safe), t(12)));
        assert_eq!(s.snapshot(t(12)).phase, Phase::Faulted);
        let (mut s, f) = ready();
        let mut future = obs(f, 4, 12, Outcome::Safe);
        future.received = t(14);
        s.observe(future, t(13));
        assert_eq!(s.snapshot(t(13)).phase, Phase::Faulted);
    }
    #[test]
    fn failed_attempt_does_not_clear_pending_unsafe_evidence() {
        let (mut s, f) = ready();
        s.policy.unsafe_readings_to_unsafe = 2;
        s.observe(obs(f, 4, 11, Outcome::Unsafe), t(11));
        s.observe(obs(f, 5, 12, Outcome::AttemptFailed), t(12));
        assert_eq!(s.snapshot(t(12)).unsafe_readings, 1);
        s.observe(obs(f, 6, 13, Outcome::Unsafe), t(13));
        assert!(!s.snapshot(t(13)).permits_safe);
    }
    #[test]
    fn aggregate_cannot_restore_with_another_source_in_grace() {
        let (a, fa) = ready();
        let (b, fb) = ready();
        let ia = Uuid::new_v4();
        let ib = Uuid::new_v4();
        let mut hub = SafetyHub::new(BTreeMap::from([(ia, a), (ib, b)]));
        assert!(hub.snapshot(t(10)).is_safe);
        hub.observe(ib, obs(fb, 4, 11, Outcome::CycleFailed), t(11));
        assert!(hub.snapshot(t(11)).is_safe);
        hub.observe(ia, obs(fa, 4, 12, Outcome::Unsafe), t(12));
        for (seq, time) in [(5, 13), (6, 18), (7, 23)] {
            hub.observe(ia, obs(fa, seq, time, Outcome::Safe), t(time));
        }
        assert!(!hub.snapshot(t(23)).is_safe);
        for (seq, time) in [(5, 24), (6, 29), (7, 34)] {
            hub.observe(ib, obs(fb, seq, time, Outcome::Safe), t(time));
        }
        assert!(hub.snapshot(t(34)).is_safe);
    }
    #[tokio::test(start_paused = true)]
    async fn scheduler_expires_permission_with_no_polls_or_getters() {
        let f = fence();
        let id = Uuid::new_v4();
        let p = SafetyPolicy {
            safe_readings_to_safe: 1,
            return_to_safe_hold_seconds: 0.0,
            maximum_safe_age_seconds: 2.0,
            ..policy()
        };
        let runtime = SafetyRuntime::new(
            SafetyHub::new(BTreeMap::from([(id, Endpoint::new(p, f).unwrap())])),
            Arc::new(MonotonicClock::default()),
        );
        let updates = runtime.subscribe();
        runtime.observe(id, obs(f, 1, 0, Outcome::Safe));
        assert!(updates.borrow().is_safe);
        tokio::time::advance(t(3)).await;
        tokio::task::yield_now().await;
        assert!(!updates.borrow().is_safe);
    }
    #[tokio::test(start_paused = true)]
    async fn resume_reconfiguration_and_shutdown_clear_cached_permission() {
        let f = fence();
        let id = Uuid::new_v4();
        let p = SafetyPolicy {
            safe_readings_to_safe: 1,
            return_to_safe_hold_seconds: 0.0,
            ..policy()
        };
        let runtime = SafetyRuntime::new(
            SafetyHub::new(BTreeMap::from([(id, Endpoint::new(p, f).unwrap())])),
            Arc::new(MonotonicClock::default()),
        );
        let updates = runtime.subscribe();
        runtime.observe(id, obs(f, 1, 0, Outcome::Safe));
        assert!(updates.borrow().is_safe);
        let next = fence();
        runtime.reset_generation(next).unwrap();
        assert!(!updates.borrow().is_safe);
        assert!(!runtime.observe(id, obs(f, 2, 0, Outcome::Safe)));
        runtime.observe(id, obs(next, 1, 0, Outcome::Safe));
        assert!(updates.borrow().is_safe);
        drop(runtime);
        assert!(!updates.borrow().is_safe);
    }
    #[test]
    fn different_membership_policies_on_one_source_remain_independent() {
        let f = fence();
        let mut immediate = Endpoint::new(
            SafetyPolicy {
                safe_readings_to_safe: 1,
                return_to_safe_hold_seconds: 0.0,
                ..policy()
            },
            f,
        )
        .unwrap();
        let mut cautious = Endpoint::new(SafetyPolicy::default(), f).unwrap();
        for time in 0..=60 {
            let observation = obs(f, time + 1, time, Outcome::Safe);
            immediate.observe(observation, t(time));
            cautious.observe(observation, t(time));
            assert!(immediate.snapshot(t(time)).permits_safe);
            assert_eq!(cautious.snapshot(t(time)).permits_safe, time == 60);
        }
    }
}
