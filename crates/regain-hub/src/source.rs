//! One bounded actor per source. Source I/O never holds an output/configuration
//! lock, and cached snapshots remain readable while a driver is stalled.
use crate::{parameters::PollPolicy, safety::Clock};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{broadcast, mpsc, oneshot, watch},
    time::{Instant, timeout},
};
use uuid::Uuid;

pub type Values = BTreeMap<String, Value>;
pub const MAX_SAMPLE_KEYS: usize = 1024;
pub const MAX_SAMPLE_TEXT_BYTES: usize = 1024 * 1024;
/// Flat typed metadata arrays share the cache's aggregate text bound. Camera
/// image buffers are not source samples and must use their own owned transport.
pub const MAX_SAMPLE_ARRAY_LENGTH: usize = 1024;
pub const MAX_SAMPLE_ARRAY_ITEMS: usize = 4096;
/// Shared admission for an entire cache or collected transport result. A
/// rejected budget is discarded; callers never publish a partially admitted set.
#[derive(Default)]
pub(crate) struct SampleBudget {
    text_bytes: usize,
    array_items: usize,
}
impl SampleBudget {
    pub(crate) fn admit(&mut self, value: &Value) -> bool {
        let values = if let Value::Array(values) = value {
            self.array_items = self.array_items.saturating_add(values.len());
            if values.len() > MAX_SAMPLE_ARRAY_LENGTH || self.array_items > MAX_SAMPLE_ARRAY_ITEMS {
                return false;
            }
            values.as_slice()
        } else {
            std::slice::from_ref(value)
        };
        for value in values {
            match value {
                Value::String(value) => {
                    self.text_bytes = self.text_bytes.saturating_add(value.len())
                }
                Value::Bool(_) | Value::Number(_) => {}
                _ => return false,
            }
            if self.text_bytes > MAX_SAMPLE_TEXT_BYTES {
                return false;
            }
        }
        true
    }
}
/// A failed measurement does not invalidate unrelated readings from the same
/// device. Ages are upstream sensor ages at request time, not HTTP cache ages.
#[derive(Clone, Debug, Default)]
pub struct SampleBatch {
    pub values: Values,
    pub errors: BTreeMap<String, SourceError>,
    pub ages_seconds: BTreeMap<String, f64>,
    /// Local virtual safety sources retain the oldest contributing safe request.
    /// None means this poll itself acquired new evidence.
    pub safety_observed_at: Option<Duration>,
    /// Merge only the supplied keys; false replaces the complete sample set.
    pub partial: bool,
    /// Another bounded request is needed to finish this polling pass.
    pub more: bool,
}
impl From<Values> for SampleBatch {
    fn from(values: Values) -> Self {
        Self {
            values,
            ..Self::default()
        }
    }
}
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SourceError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionMethod {
    Legacy,
    Async,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub device_type: crate::config::DeviceType,
    pub interface_version: Option<u16>,
    pub method: ConnectionMethod,
    pub owns_connection: bool,
    pub uncertain: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    Connecting,
    Disconnected,
    Busy,
    Transient,
    Permanent,
    Uncertain,
    Unsupported,
    InvalidValue,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceError {
    pub kind: ErrorKind,
    pub message: &'static str,
    pub upstream_code: Option<i32>,
    #[serde(skip)]
    pub retry_after: Option<Duration>,
    /// The adapter can no longer safely match responses to requests. Ordinary
    /// HTTP errors do not imply a lost session; a cancelled serial read does.
    #[serde(skip)]
    pub transport_lost: bool,
}
impl SourceError {
    pub fn new(kind: ErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            message,
            upstream_code: None,
            retry_after: None,
            transport_lost: false,
        }
    }
    pub fn transient() -> Self {
        Self::new(ErrorKind::Transient, "Source request failed or timed out")
    }
    pub fn timeout() -> Self {
        Self {
            transport_lost: true,
            ..Self::transient()
        }
    }
    pub fn uncertain() -> Self {
        Self::new(
            ErrorKind::Uncertain,
            "The write may have reached the device; reconcile its state before another command",
        )
    }
}
impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for SourceError {}

/// Implementations own their connection policy. Externally managed sources must
/// not be disconnected, and a worker reset must stop any local in-flight I/O.
pub trait Backend: Send {
    fn simulated(&self) -> bool {
        false
    }
    fn simulation_status(&self) -> Option<crate::simulated::SimulationStatus> {
        None
    }
    fn update_simulation(
        &mut self,
        _: crate::simulated::SimulationUpdate,
    ) -> Result<crate::simulated::SimulationStatus, SourceError> {
        Err(SourceError::new(
            ErrorKind::Unsupported,
            "This source is not an adjustable simulator",
        ))
    }
    fn connect(&mut self) -> BackendFuture<'_, ()>;
    /// One bounded handshake step. False means pending, not a failed poll cycle.
    fn connect_step(&mut self) -> BackendFuture<'_, bool> {
        Box::pin(async { self.connect().await.map(|()| true) })
    }
    fn connection_info(&self) -> Option<ConnectionInfo> {
        None
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()>;
    /// After command admission closes and disconnect/reset fences the source,
    /// join backend-owned retirement. This must not dispatch or retry equipment
    /// commands. A disconnect timeout retains its original uncertain result;
    /// it does not prove that asynchronously owned cleanup has finished.
    fn finish_shutdown(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn read(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value>;
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value>;
    /// Binary images bypass scalar sampling and its JSON/array limits. Only the
    /// acquisition supervisor holding source control may dispatch this read.
    /// Implementations must reserve from the supplied shared budget before an
    /// image-sized allocation; the source actor bounds the whole operation.
    fn camera_image(
        &mut self,
        _: crate::camera::image::ImageBudget,
    ) -> BackendFuture<'_, crate::camera::image::CameraImage> {
        Box::pin(async {
            Err(SourceError::new(
                ErrorKind::Unsupported,
                "This source does not provide camera images",
            ))
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values>;
    fn sample(&mut self) -> BackendFuture<'_, SampleBatch> {
        Box::pin(async { self.poll().await.map(SampleBatch::from) })
    }
    fn reset(&mut self);
    fn restart_poll(&mut self) {}
    /// Trigger sensor acquisition without waiting for measurements. Sources
    /// without an upstream acquisition command just restart their local poll.
    fn refresh(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PollPhase {
    Idle,
    Connecting,
    Sampling,
    Waiting,
    Suspended,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PollReason {
    Initial,
    Connection,
    Retry,
    Periodic,
    Continuation,
    Refresh,
    StateChanged,
}

/// An actor observation, not a live countdown or an equipment command retry.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PollingStatus {
    pub phase: PollPhase,
    /// Local monotonic time when the actor published this observation.
    #[schemars(range(min = 0.0))]
    pub observed_seconds: f64,
    pub reason: Option<PollReason>,
    /// Remaining wait at observedSeconds. Other actor work may delay dispatch.
    /// Null while inactive/in flight, or if the deadline is unrepresentable.
    #[schemars(range(min = 0.0))]
    pub next_poll_after_seconds: Option<f64>,
    /// Started attempts in the current cycle, including an in-flight request.
    /// Zero after cycle completion or exhaustion.
    pub attempts_started: u32,
    #[schemars(range(min = 1, max = 10))]
    pub attempts_per_cycle: u32,
    pub last_attempt: u32,
    pub last_cycle_exhausted: Option<bool>,
    pub backoff_failures: u32,
}
impl PollingStatus {
    fn idle(policy: &PollPolicy, now: Duration) -> Self {
        Self {
            phase: PollPhase::Idle,
            observed_seconds: now.as_secs_f64(),
            reason: None,
            next_poll_after_seconds: None,
            attempts_started: 0,
            attempts_per_cycle: policy.attempts_per_cycle,
            last_attempt: 0,
            last_cycle_exhausted: None,
            backoff_failures: 0,
        }
    }
}
impl Default for PollingStatus {
    fn default() -> Self {
        Self::idle(&PollPolicy::default(), Duration::ZERO)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSnapshot {
    pub source: Uuid,
    pub revision: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub transport_connected: bool,
    pub write_uncertain: bool,
    pub connection_info: Option<ConnectionInfo>,
    pub simulated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulation: Option<crate::simulated::SimulationStatus>,
    pub lease_count: usize,
    pub values: Values,
    pub sample_errors: BTreeMap<String, SourceError>,
    pub sample_ages_seconds: BTreeMap<String, f64>,
    pub sample_started_seconds: BTreeMap<String, f64>,
    pub sample_sequences: BTreeMap<String, u64>,
    pub completed_passes: u64,
    pub sampled_at_seconds: Option<f64>,
    pub error: Option<SourceError>,
    pub polling: PollingStatus,
}
impl SourceSnapshot {
    /// A cached scalar reader needs only its selected keys. Do not copy vendor
    /// text, connection data or simulation controls into a diagnostic engine.
    pub(crate) fn project_samples(&self, keys: &BTreeSet<String>) -> Self {
        fn selected<T: Clone>(
            values: &BTreeMap<String, T>,
            keys: &BTreeSet<String>,
        ) -> BTreeMap<String, T> {
            keys.iter()
                .filter_map(|key| values.get(key).map(|value| (key.clone(), value.clone())))
                .collect()
        }
        Self {
            source: self.source,
            revision: self.revision,
            generation: self.generation,
            sequence: self.sequence,
            transport_connected: self.transport_connected,
            write_uncertain: self.write_uncertain,
            connection_info: None,
            simulated: self.simulated,
            simulation: None,
            lease_count: self.lease_count,
            values: keys
                .iter()
                .filter_map(|key| {
                    self.values
                        .get(key)
                        .filter(|value| value.is_number() || value.is_boolean())
                        .map(|value| (key.clone(), value.clone()))
                })
                .collect(),
            sample_errors: selected(&self.sample_errors, keys),
            sample_ages_seconds: selected(&self.sample_ages_seconds, keys),
            sample_started_seconds: selected(&self.sample_started_seconds, keys),
            sample_sequences: selected(&self.sample_sequences, keys),
            completed_passes: self.completed_passes,
            sampled_at_seconds: self.sampled_at_seconds,
            error: self.error.clone(),
            polling: self.polling.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PollEvent {
    pub source: Uuid,
    pub revision: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub started: Duration,
    pub received: Duration,
    pub result: Result<Values, SourceError>,
    pub cycle_exhausted: bool,
}

type Reply<T> = oneshot::Sender<Result<T, SourceError>>;
enum Command {
    CameraImage {
        lease: Uuid,
        expected_generation: Uuid,
        budget: crate::camera::image::ImageBudget,
        deadline: Duration,
        reply: Reply<crate::camera::image::CameraImage>,
    },
    UpdateSimulation {
        lease: Uuid,
        update: Box<crate::simulated::SimulationUpdate>,
        reply: Reply<crate::simulated::SimulationStatus>,
    },
    Shutdown,
    Refresh {
        lease: Uuid,
        reply: Reply<()>,
    },
    Acquire {
        lease: Uuid,
        reply: Reply<SourceSnapshot>,
    },
    Release {
        lease: Uuid,
        reply: Reply<()>,
    },
    Control {
        lease: Uuid,
        acquire: bool,
        reply: Reply<()>,
    },
    Read {
        lease: Uuid,
        expected_generation: Option<Uuid>,
        member: String,
        parameters: Values,
        reply: Reply<Value>,
    },
    Write {
        lease: Uuid,
        expected_generation: Option<Uuid>,
        member: String,
        parameters: Values,
        reply: Reply<Value>,
    },
}

pub struct SourceHandle {
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<SourceSnapshot>,
    events: broadcast::Sender<PollEvent>,
    completion: watch::Receiver<Option<Result<(), SourceError>>>,
}

/// One immutable registry per applied configuration revision. Construction is
/// transactional and does no device I/O; host configuration replacement owns
/// the disconnected-output check before retiring an old registry.
pub struct SourceRegistry {
    sources: BTreeMap<Uuid, Arc<SourceHandle>>,
}
impl SourceRegistry {
    pub fn build(
        config: &crate::config::HubConfig,
        clock: Arc<dyn Clock>,
        mut factory: impl FnMut(&crate::config::SourceConfig) -> Result<Box<dyn Backend>, SourceError>,
    ) -> Result<Self, Vec<crate::parameters::FieldError>> {
        use crate::parameters::FieldError;
        let errors = config.validate();
        if !errors.is_empty() {
            return Err(errors);
        }
        // Prepare every adapter before spawning actors. A failed adapter must
        // not leave a partially usable registry or an open device connection.
        let mut prepared = Vec::new();
        for (index, source) in config.sources.iter().enumerate() {
            let backend = factory(source).map_err(|error| {
                vec![FieldError::new(
                    format!("sources[{index}].backend"),
                    "backend",
                    error.message,
                )]
            })?;
            prepared.push((source, backend));
        }
        let mut sources = BTreeMap::new();
        for (source, backend) in prepared {
            sources.insert(
                source.id,
                SourceHandle::spawn(
                    source.id,
                    config.revision,
                    source.polling.clone(),
                    backend,
                    clock.clone(),
                )?,
            );
        }
        Ok(Self { sources })
    }
    pub fn get(&self, source: Uuid) -> Result<Arc<SourceHandle>, SourceError> {
        self.sources
            .get(&source)
            .cloned()
            .ok_or_else(|| SourceError::new(ErrorKind::InvalidValue, "Unknown source ID"))
    }
    pub fn snapshots(&self) -> Vec<SourceSnapshot> {
        self.sources
            .values()
            .map(|source| source.snapshot())
            .collect()
    }
    /// Stop all actors concurrently, including actors still held by old output
    /// guards. Collect every cleanup result; one failure cannot skip another.
    pub async fn shutdown(&self) -> Result<(), Vec<(Uuid, SourceError)>> {
        let mut tasks = tokio::task::JoinSet::new();
        for (id, source) in &self.sources {
            let id = *id;
            let source = source.clone();
            tasks.spawn(async move { (id, source.shutdown().await) });
        }
        let mut errors = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((id, Err(error))) => errors.push((id, error)),
                Ok((_, Ok(()))) => {}
                Err(_) => errors.push((Uuid::nil(), closed())),
            }
        }
        errors.sort_by_key(|(id, _)| *id);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl SourceHandle {
    pub fn spawn(
        source: Uuid,
        revision: Uuid,
        policy: PollPolicy,
        backend: Box<dyn Backend>,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, Vec<crate::parameters::FieldError>> {
        let mut errors = policy.validate_timing();
        if source.is_nil() || revision.is_nil() {
            errors.push(crate::parameters::FieldError::new(
                "source",
                "identity",
                "Source and revision IDs cannot be nil",
            ));
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        let initial = SourceSnapshot {
            source,
            revision,
            generation: Uuid::new_v4(),
            sequence: 0,
            transport_connected: false,
            write_uncertain: false,
            connection_info: None,
            simulated: backend.simulated(),
            simulation: backend.simulation_status(),
            lease_count: 0,
            values: Values::new(),
            sample_errors: BTreeMap::new(),
            sample_ages_seconds: BTreeMap::new(),
            sample_started_seconds: BTreeMap::new(),
            sample_sequences: BTreeMap::new(),
            completed_passes: 0,
            sampled_at_seconds: None,
            error: None,
            polling: PollingStatus::idle(&policy, clock.now()),
        };
        let (commands, receiver) = mpsc::channel(16);
        let (snapshot, reader) = watch::channel(initial.clone());
        let (events, _) = broadcast::channel(64);
        let (completion, completed) = watch::channel(None);
        let handle = Arc::new(Self {
            commands,
            snapshot: reader,
            events: events.clone(),
            completion: completed,
        });
        tokio::spawn(
            Actor {
                backend,
                policy,
                clock,
                snapshot,
                events,
                state: initial,
                leases: BTreeSet::new(),
                controller: None,
                write_uncertain: false,
                next_poll: Some(Instant::now()),
                attempt: 0,
                backoff_failures: 0,
                retrying: false,
                poll_operation: None,
                poll_reason: Some(PollReason::Initial),
                last_attempt: 0,
                last_cycle_exhausted: None,
                stopped: false,
                connection_started: None,
                // Construction opens no device; an unused prepared runtime has
                // no backend cleanup to dispatch when validation is abandoned.
                disconnect_result: Some(Ok(())),
                completion,
            }
            .run(receiver),
        );
        Ok(handle)
    }
    pub fn snapshot(&self) -> SourceSnapshot {
        self.snapshot.borrow().clone()
    }
    pub(crate) fn with_snapshot<T>(&self, read: impl FnOnce(&SourceSnapshot) -> T) -> T {
        read(&self.snapshot.borrow())
    }
    /// The terminal cleanup result is retained, making concurrent/repeated
    /// shutdown idempotent without replaying an upstream Disconnect.
    pub async fn shutdown(&self) -> Result<(), SourceError> {
        let mut completion = self.completion.clone();
        if completion.borrow().is_none() {
            // If already closing, wait for its terminal result below.
            let _ = self.commands.send(Command::Shutdown).await;
        }
        loop {
            if let Some(result) = completion.borrow_and_update().clone() {
                return result;
            }
            completion.changed().await.map_err(|_| closed())?;
        }
    }
    /// Generation changes invalidate observations even before a poll finishes.
    pub fn status(&self) -> watch::Receiver<SourceSnapshot> {
        self.snapshot.clone()
    }
    /// A lagging consumer MUST invalidate its evidence on RecvError::Lagged.
    pub fn subscribe(&self) -> broadcast::Receiver<PollEvent> {
        self.events.subscribe()
    }
    pub async fn acquire(&self, lease: Uuid) -> Result<SourceSnapshot, SourceError> {
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::Acquire { lease, reply })?;
        response.await.map_err(|_| closed())?
    }
    pub async fn release(&self, lease: Uuid) -> Result<(), SourceError> {
        let (reply, response) = oneshot::channel();
        // Cleanup must not be dropped merely because ordinary requests filled
        // the bounded queue. Each running request has a backend deadline.
        self.commands
            .send(Command::Release { lease, reply })
            .await
            .map_err(|_| closed())?;
        response.await.map_err(|_| closed())?
    }
    pub async fn control(&self, lease: Uuid, acquire: bool) -> Result<(), SourceError> {
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::Control {
            lease,
            acquire,
            reply,
        })?;
        response.await.map_err(|_| closed())?
    }
    pub async fn update_simulation(
        &self,
        lease: Uuid,
        update: crate::simulated::SimulationUpdate,
    ) -> Result<crate::simulated::SimulationStatus, SourceError> {
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::UpdateSimulation {
            lease,
            update: Box::new(update),
            reply,
        })?;
        response.await.map_err(|_| closed())?
    }
    /// Acknowledge the refresh trigger, not completion of a sampling pass.
    pub async fn refresh(&self, lease: Uuid) -> Result<(), SourceError> {
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::Refresh { lease, reply })?;
        response.await.map_err(|_| closed())?
    }
    pub async fn read(
        &self,
        lease: Uuid,
        member: &str,
        parameters: Values,
    ) -> Result<Value, SourceError> {
        self.read_fenced(lease, member, parameters, None).await
    }
    pub async fn read_fenced(
        &self,
        lease: Uuid,
        member: &str,
        parameters: Values,
        expected_generation: Option<Uuid>,
    ) -> Result<Value, SourceError> {
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::Read {
            lease,
            expected_generation,
            member: member.into(),
            parameters,
            reply,
        })?;
        response.await.map_err(|_| closed())?
    }
    pub async fn write(
        &self,
        lease: Uuid,
        member: &str,
        parameters: Values,
    ) -> Result<Value, SourceError> {
        self.write_fenced(lease, member, parameters, None).await
    }
    /// Read once on behalf of the exclusive acquisition owner. Completed-image
    /// observers use the supervisor's immutable image, not additional leaf I/O.
    /// Generation fencing is mandatory; this never starts/retries an exposure.
    pub async fn camera_image_fenced(
        &self,
        lease: Uuid,
        expected_generation: Uuid,
        budget: crate::camera::image::ImageBudget,
        deadline: Duration,
    ) -> Result<crate::camera::image::CameraImage, SourceError> {
        // Camera recovery metadata supplies this separately from scalar polling.
        // Match the existing core download-timeout bound without changing it.
        if deadline.is_zero() || deadline > Duration::from_secs(3600) {
            return Err(SourceError::new(
                ErrorKind::InvalidValue,
                "Camera image download deadline must be positive and at most one hour",
            ));
        }
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::CameraImage {
            lease,
            expected_generation,
            budget,
            deadline,
            reply,
        })?;
        response.await.map_err(|_| closed())?
    }
    pub async fn write_fenced(
        &self,
        lease: Uuid,
        member: &str,
        parameters: Values,
        expected_generation: Option<Uuid>,
    ) -> Result<Value, SourceError> {
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::Write {
            lease,
            expected_generation,
            member: member.into(),
            parameters,
            reply,
        })?;
        response.await.map_err(|_| closed())?
    }
    fn enqueue(&self, command: Command) -> Result<(), SourceError> {
        self.commands.try_send(command).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                SourceError::new(ErrorKind::Busy, "Source command queue is full")
            }
            mpsc::error::TrySendError::Closed(_) => closed(),
        })
    }
}
fn closed() -> SourceError {
    SourceError::new(ErrorKind::Disconnected, "Source actor stopped")
}

struct Actor {
    backend: Box<dyn Backend>,
    policy: PollPolicy,
    clock: Arc<dyn Clock>,
    snapshot: watch::Sender<SourceSnapshot>,
    events: broadcast::Sender<PollEvent>,
    state: SourceSnapshot,
    leases: BTreeSet<Uuid>,
    controller: Option<Uuid>,
    write_uncertain: bool,
    next_poll: Option<Instant>,
    attempt: u32,
    backoff_failures: u32,
    retrying: bool,
    poll_operation: Option<PollPhase>,
    poll_reason: Option<PollReason>,
    last_attempt: u32,
    last_cycle_exhausted: Option<bool>,
    stopped: bool,
    connection_started: Option<Instant>,
    disconnect_result: Option<Result<(), SourceError>>,
    completion: watch::Sender<Option<Result<(), SourceError>>>,
}
impl Actor {
    fn deadline(&self) -> Duration {
        Duration::from_secs_f64(self.policy.request_timeout_seconds)
    }
    fn publish(&mut self) {
        self.state.connection_info = self.backend.connection_info();
        self.state.simulation = self.backend.simulation_status();
        self.state.write_uncertain = self.write_uncertain;
        self.state.lease_count = self.leases.len();
        let phase = if self.stopped {
            PollPhase::Stopped
        } else if let Some(operation) = self.poll_operation {
            operation
        } else if self.leases.is_empty() {
            PollPhase::Idle
        } else if self.next_poll.is_some() {
            PollPhase::Waiting
        } else {
            PollPhase::Suspended
        };
        self.state.polling = PollingStatus {
            phase,
            observed_seconds: self.clock.now().as_secs_f64(),
            reason: (!matches!(phase, PollPhase::Idle | PollPhase::Stopped))
                .then_some(self.poll_reason)
                .flatten(),
            next_poll_after_seconds: (phase == PollPhase::Waiting)
                .then(|| {
                    self.next_poll.map(|deadline| {
                        deadline
                            .saturating_duration_since(Instant::now())
                            .as_secs_f64()
                    })
                })
                .flatten(),
            attempts_started: self.attempt,
            attempts_per_cycle: self.policy.attempts_per_cycle,
            last_attempt: self.last_attempt,
            last_cycle_exhausted: self.last_cycle_exhausted,
            backoff_failures: self.backoff_failures,
        };
        self.snapshot.send_replace(self.state.clone());
    }
    fn fault(&mut self, error: SourceError) {
        self.connection_started = None;
        self.state.error = Some(error);
        self.state.transport_connected = false;
        self.state.generation = Uuid::new_v4();
        self.state.sequence = 0;
        self.state.sample_started_seconds.clear();
        self.state.sample_sequences.clear();
        self.state.values.clear();
        self.state.sample_errors.clear();
        self.state.sample_ages_seconds.clear();
        self.state.sampled_at_seconds = None;
        self.backend.reset();
        self.publish();
    }
    async fn connect(&mut self) -> Result<(), SourceError> {
        if self.state.transport_connected {
            return Ok(());
        }
        let started = *self.connection_started.get_or_insert_with(Instant::now);
        let remaining = Duration::from_secs_f64(self.policy.connection_timeout_seconds)
            .saturating_sub(started.elapsed());
        self.poll_operation = Some(PollPhase::Connecting);
        self.publish();
        let result = if remaining.is_zero() {
            Err(SourceError::timeout())
        } else {
            timeout(self.deadline().min(remaining), self.backend.connect_step())
                .await
                .unwrap_or_else(|_| Err(SourceError::timeout()))
        };
        self.poll_operation = None;
        match result {
            Ok(false) => {
                let error =
                    SourceError::new(ErrorKind::Connecting, "Source connection is in progress");
                self.state.error = Some(error.clone());
                self.publish();
                Err(error)
            }
            Ok(true) => {
                self.connection_started = None;
                self.state.transport_connected = true;
                // New transport cannot inherit another worker's safe evidence.
                self.state.generation = Uuid::new_v4();
                self.state.sequence = 0;
                self.state.sample_started_seconds.clear();
                self.state.sample_sequences.clear();
                self.state.values.clear();
                self.state.sample_errors.clear();
                self.state.sample_ages_seconds.clear();
                self.state.sampled_at_seconds = None;
                self.state.error = None;
                self.publish();
                Ok(())
            }
            Err(e) => {
                // Some handshakes initialize settings. An unknown write outcome
                // during connection needs the same last-lease fence as a command.
                if e.kind == ErrorKind::Uncertain {
                    self.write_uncertain = true;
                }
                self.fault(e.clone());
                Err(e)
            }
        }
    }
    async fn disconnect(&mut self) -> Result<(), SourceError> {
        // Last-lease release may already have attempted cleanup before the
        // queued shutdown arrives. Preserve that result, including uncertainty.
        if let Some(result) = &self.disconnect_result {
            return result.clone();
        }
        self.connection_started = None;
        let result = timeout(self.deadline(), self.backend.disconnect())
            .await
            .unwrap_or_else(|_| Err(SourceError::uncertain()));
        if result.is_err() {
            self.backend.reset();
        }
        self.state.transport_connected = false;
        self.state.values.clear();
        self.state.sample_errors.clear();
        self.state.sample_ages_seconds.clear();
        self.state.sampled_at_seconds = None;
        self.state.generation = Uuid::new_v4();
        self.state.sequence = 0;
        self.state.sample_started_seconds.clear();
        self.state.sample_sequences.clear();
        self.controller = None;
        self.write_uncertain = false;
        self.attempt = 0;
        self.backoff_failures = 0;
        self.retrying = false;
        self.next_poll = None;
        self.poll_reason = None;
        self.last_attempt = 0;
        self.last_cycle_exhausted = None;
        self.backend.restart_poll();
        self.state.error = result.as_ref().err().cloned();
        self.disconnect_result = Some(result.clone());
        self.publish();
        result
    }
    fn authorized(&self, lease: Uuid, write: bool) -> Result<(), SourceError> {
        if !self.leases.contains(&lease) {
            return Err(SourceError::new(
                ErrorKind::Disconnected,
                "This client has no source connection lease",
            ));
        }
        if write && self.write_uncertain {
            return Err(SourceError::uncertain());
        }
        if write && self.controller != Some(lease) {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "Acquire the source control lease before writing",
            ));
        }
        Ok(())
    }
    async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        loop {
            tokio::select! {
                // Due sampling cannot be starved by clients flooding the queue.
                biased;
                _=wait_until(self.next_poll), if !self.leases.is_empty() => self.poll().await,
                command=commands.recv() => match command {
                    Some(Command::Shutdown) | None => break,
                    Some(command)=>self.command(command).await,
                }
            }
        }
        commands.close();
        // Release queued request/reply pairs before cleanup can wait on I/O.
        while commands.try_recv().is_ok() {}
        self.leases.clear();
        let result = self.disconnect().await;
        // Keep the actor's completion (and therefore host ownership) retained
        // until backend tasks actually stop using their worker/OS resources.
        let drained = self.backend.finish_shutdown().await;
        let result = result.and(drained);
        self.state.error = Some(result.as_ref().err().cloned().unwrap_or_else(closed));
        self.stopped = true;
        self.publish();
        self.completion.send_replace(Some(result));
    }
    async fn command(&mut self, command: Command) {
        match command {
            Command::UpdateSimulation {
                lease,
                update,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let result = self.authorized(lease, false).and_then(|()| {
                    if self.controller != Some(lease) {
                        return Err(SourceError::new(
                            ErrorKind::Busy,
                            "Acquire source control before changing simulation",
                        ));
                    }
                    self.backend.update_simulation(*update)
                });
                if result.is_ok() {
                    self.state.sampled_at_seconds = None;
                    self.state.values.clear();
                    self.state.sample_errors.clear();
                    self.state.sample_ages_seconds.clear();
                    self.state.sample_started_seconds.clear();
                    self.retrying = false;
                    self.attempt = 0;
                    self.backoff_failures = 0;
                    self.backend.restart_poll();
                    self.next_poll = Some(Instant::now());
                    self.poll_reason = Some(PollReason::StateChanged);
                    // Do not clear an uncertain-write latch through test controls.
                    self.publish();
                }
                let _ = reply.send(result);
            }
            Command::Shutdown => unreachable!("Handled by the actor loop"),
            Command::Refresh { lease, reply } => {
                if reply.is_closed() {
                    return;
                }
                let result = self.authorized(lease, false).and_then(|()| {
                    if self.retrying
                        && self
                            .next_poll
                            .is_none_or(|deadline| deadline > Instant::now())
                    {
                        return Err(SourceError {
                            retry_after: self
                                .next_poll
                                .map(|deadline| deadline.saturating_duration_since(Instant::now())),
                            ..SourceError::new(ErrorKind::Busy, "Source is waiting before retrying")
                        });
                    }
                    if !self.state.transport_connected {
                        return Err(SourceError::new(
                            ErrorKind::Disconnected,
                            "Source is disconnected",
                        ));
                    }
                    Ok(())
                });
                let result = match result {
                    Ok(()) => timeout(self.deadline(), self.backend.refresh())
                        .await
                        .unwrap_or_else(|_| Err(SourceError::timeout())),
                    Err(error) => Err(error),
                };
                match &result {
                    Ok(()) => {
                        self.backend.restart_poll();
                        self.next_poll = Some(Instant::now());
                        self.poll_reason = Some(PollReason::Refresh);
                    }
                    Err(error) if error.kind != ErrorKind::Busy => {
                        if error.transport_lost {
                            self.fault(error.clone());
                        }
                        if matches!(
                            error.kind,
                            ErrorKind::Transient | ErrorKind::Disconnected | ErrorKind::Uncertain
                        ) {
                            self.retrying = true;
                            self.poll_reason = Some(PollReason::Retry);
                            self.next_poll = Instant::now().checked_add(
                                error
                                    .retry_after
                                    .unwrap_or_default()
                                    .max(Duration::from_secs_f64(
                                        self.policy.initial_backoff_seconds,
                                    )),
                            );
                        }
                    }
                    _ => {}
                }
                self.publish();
                let _ = reply.send(result);
            }
            Command::Acquire { lease, reply } => {
                if reply.is_closed() {
                    return;
                }
                if lease.is_nil() {
                    let _ = reply.send(Err(SourceError::new(
                        ErrorKind::InvalidValue,
                        "Lease ID cannot be nil",
                    )));
                    return;
                }
                let inserted = self.leases.insert(lease);
                let first = inserted && self.leases.len() == 1;
                if first {
                    self.disconnect_result = None;
                    let delay = match self.connect().await {
                        Ok(()) => Duration::ZERO,
                        Err(error) => {
                            self.retrying = true;
                            if error.kind == ErrorKind::Connecting {
                                Duration::from_millis(100)
                            } else {
                                error
                                    .retry_after
                                    .unwrap_or_default()
                                    .max(Duration::from_secs_f64(
                                        if matches!(
                                            error.kind,
                                            ErrorKind::Transient | ErrorKind::Disconnected
                                        ) {
                                            self.policy.initial_backoff_seconds
                                        } else {
                                            self.policy.backoff_cap_seconds
                                        },
                                    ))
                            }
                        }
                    };
                    self.next_poll = Instant::now().checked_add(delay);
                    self.poll_reason = Some(if self.connection_started.is_some() {
                        PollReason::Connection
                    } else if self.retrying {
                        PollReason::Retry
                    } else {
                        PollReason::Initial
                    });
                }
                // Offline sources retain their lease and are polled for recovery.
                // A SafetyMonitor output can remain connected while unsafe.
                self.publish();
                if reply.send(Ok(self.state.clone())).is_err() && inserted {
                    self.leases.remove(&lease);
                    if self.leases.is_empty() {
                        let _ = self.disconnect().await;
                    } else {
                        self.publish();
                    }
                }
            }
            Command::Release { lease, reply } => {
                if self.leases.remove(&lease) && self.leases.is_empty() {
                    let _ = self.disconnect().await;
                }
                if self.controller == Some(lease) {
                    self.controller = None;
                }
                self.publish();
                let _ = reply.send(Ok(()));
            }
            Command::Control {
                lease,
                acquire,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let previous_controller = self.controller;
                let result = self.authorized(lease, false).and_then(|()| {
                    if acquire {
                        if self.controller.is_some_and(|owner| owner != lease) {
                            return Err(SourceError::new(
                                ErrorKind::Busy,
                                "Another client owns source control",
                            ));
                        }
                        self.controller = Some(lease);
                    } else if self.controller == Some(lease) {
                        self.controller = None;
                    }
                    Ok(())
                });
                // A cancelled claim cannot leave a client holding control.
                if reply.send(result).is_err() && acquire && self.controller == Some(lease) {
                    self.controller = previous_controller;
                }
            }
            Command::CameraImage {
                lease,
                expected_generation,
                budget,
                deadline,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let mut dispatched = false;
                let admission = self.authorized(lease, false).and_then(|()| {
                    if self.controller != Some(lease) {
                        Err(SourceError::new(
                            ErrorKind::Busy,
                            "Acquire source control before downloading a camera image",
                        ))
                    } else {
                        Ok(())
                    }
                });
                let result = match admission {
                    Err(error) => Err(error),
                    Ok(()) => match self.connect().await {
                        Err(error) => Err(error),
                        Ok(()) if expected_generation != self.state.generation => {
                            Err(SourceError::new(
                                ErrorKind::Unavailable,
                                "Source generation changed before image dispatch",
                            ))
                        }
                        Ok(()) => {
                            dispatched = true;
                            timeout(deadline, self.backend.camera_image(budget))
                                .await
                                .unwrap_or_else(|_| Err(SourceError::timeout()))
                        }
                    },
                };
                if let Err(error) = &result
                    && dispatched
                    && error.transport_lost
                {
                    self.fault(error.clone());
                }
                // No sample/cache update, command replay or uncertainty reset.
                // Dropping a disconnected receiver releases its image handle.
                let _ = reply.send(result);
            }
            Command::Read {
                lease,
                expected_generation,
                member,
                parameters,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let mut dispatched = false;
                let result = match self.authorized(lease, false) {
                    Err(e) => Err(e),
                    Ok(()) => match self.connect().await {
                        Err(e) => Err(e),
                        Ok(()) => {
                            if expected_generation
                                .is_some_and(|generation| generation != self.state.generation)
                            {
                                let _ = reply.send(Err(SourceError::new(
                                    ErrorKind::Unavailable,
                                    "Source generation changed before dispatch",
                                )));
                                return;
                            }
                            dispatched = true;
                            timeout(self.deadline(), self.backend.read(member, parameters))
                                .await
                                .unwrap_or_else(|_| Err(SourceError::timeout()))
                        }
                    },
                };
                if let Err(e) = &result
                    && dispatched
                    && e.transport_lost
                {
                    self.fault(e.clone());
                }
                let _ = reply.send(result);
            }
            Command::Write {
                lease,
                expected_generation,
                member,
                parameters,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let mut dispatched = false;
                let mut result = match self.authorized(lease, true) {
                    Err(e) => Err(e),
                    Ok(()) => match self.connect().await {
                        Err(e) => Err(e),
                        Ok(()) => {
                            if expected_generation
                                .is_some_and(|generation| generation != self.state.generation)
                            {
                                let _ = reply.send(Err(SourceError::new(
                                    ErrorKind::Unavailable,
                                    "Source generation changed before dispatch",
                                )));
                                return;
                            }
                            dispatched = true;
                            timeout(self.deadline(), self.backend.write(member, parameters))
                                .await
                                .unwrap_or_else(|_| Err(SourceError::uncertain()))
                        }
                    },
                };
                if dispatched
                    && result.as_ref().is_err_and(|e| {
                        matches!(e.kind, ErrorKind::Transient | ErrorKind::Uncertain)
                    })
                {
                    self.write_uncertain = true;
                    let error = SourceError {
                        kind: ErrorKind::Uncertain,
                        message: SourceError::uncertain().message,
                        ..result.unwrap_err()
                    };
                    self.fault(error.clone());
                    result = Err(error);
                } else if let Err(error) = &result
                    && dispatched
                    && error.transport_lost
                {
                    // A definitive rejection can retire the transport without
                    // making the command ambiguous.
                    self.fault(error.clone());
                }
                if dispatched && result.is_ok() {
                    // A successful command can change the cached device state.
                    // Confirm with a new poll instead of reporting the old value
                    // or inventing an optimistic value for an asynchronous device.
                    self.state.sampled_at_seconds = None;
                    self.state.values.clear();
                    self.state.sample_errors.clear();
                    self.state.sample_ages_seconds.clear();
                    self.state.sample_started_seconds.clear();
                    self.backend.restart_poll();
                    if !self.retrying {
                        self.next_poll = Some(Instant::now());
                        self.poll_reason = Some(PollReason::StateChanged);
                    }
                    self.publish();
                }
                let _ = reply.send(result);
            }
        }
    }
    async fn poll(&mut self) {
        let started = self.clock.now();
        let connection = self.connect().await;
        if connection
            .as_ref()
            .is_err_and(|error| error.kind == ErrorKind::Connecting)
        {
            self.next_poll = Some(Instant::now() + Duration::from_millis(100));
            self.poll_reason = Some(PollReason::Connection);
            self.publish();
            return;
        }
        self.attempt += 1;
        self.last_attempt = self.attempt;
        self.last_cycle_exhausted = None;
        let sampled = connection.is_ok();
        if sampled {
            self.poll_operation = Some(PollPhase::Sampling);
            self.publish();
        }
        let result = match connection {
            Err(e) => Err(e),
            Ok(()) => timeout(self.deadline(), self.backend.sample())
                .await
                .unwrap_or_else(|_| Err(SourceError::timeout())),
        }
        .and_then(|batch| {
            validate_batch(&self.state, &batch)?;
            Ok(batch)
        });
        self.poll_operation = None;
        let received = self.clock.now();
        let observed = result.as_ref().ok().and_then(|b| b.safety_observed_at);
        // Local adapters use the same monotonic clock. Never allow a delegated
        // observation to be newer than the enclosing request.
        let event_started = observed.map_or(started, |at| at.min(started));
        let retry_error = result
            .as_ref()
            .err()
            .filter(|error| matches!(error.kind, ErrorKind::Transient | ErrorKind::Disconnected))
            .or_else(|| {
                result.as_ref().ok().and_then(|batch| {
                    batch
                        .errors
                        .values()
                        .filter(|error| {
                            matches!(error.kind, ErrorKind::Transient | ErrorKind::Disconnected)
                        })
                        .max_by_key(|error| error.retry_after)
                })
            })
            .cloned();
        let retryable = retry_error.is_some();
        let more = result.as_ref().is_ok_and(|batch| batch.more);
        let exhausted = !retryable || self.attempt >= self.policy.attempts_per_cycle;
        self.last_cycle_exhausted = Some(exhausted && !more);
        if let Err(error) = &result
            && sampled
            && error.transport_lost
        {
            self.fault(error.clone());
        }
        self.state.sequence = self
            .state
            .sequence
            .checked_add(1)
            .expect("Source sequence exhausted");
        let event = PollEvent {
            source: self.state.source,
            revision: self.state.revision,
            generation: self.state.generation,
            sequence: self.state.sequence,
            started: event_started,
            received,
            result: result.clone().map(|batch| batch.values),
            cycle_exhausted: exhausted,
        };
        match &result {
            Ok(batch) => {
                if !batch.partial {
                    self.state.values.clear();
                    self.state.sample_errors.clear();
                    self.state.sample_ages_seconds.clear();
                    self.state.sample_started_seconds.clear();
                }
                for (key, value) in &batch.values {
                    self.state.values.insert(key.clone(), value.clone());
                    self.state.sample_errors.remove(key);
                    self.state.sample_ages_seconds.insert(
                        key.clone(),
                        batch.ages_seconds.get(key).copied().unwrap_or(0.0),
                    );
                    self.state
                        .sample_started_seconds
                        .insert(key.clone(), started.as_secs_f64());
                    let sequence = self.state.sample_sequences.entry(key.clone()).or_default();
                    *sequence = sequence.saturating_add(1);
                }
                for (key, error) in &batch.errors {
                    self.state.values.remove(key);
                    self.state.sample_started_seconds.remove(key);
                    self.state.sample_ages_seconds.remove(key);
                    self.state.sample_errors.insert(key.clone(), error.clone());
                }
                self.state.sampled_at_seconds = Some(started.as_secs_f64());
                self.state.error = None;
                if !retryable {
                    self.backoff_failures = 0;
                }
            }
            Err(error) => {
                self.state.error = Some(error.clone());
            }
        }
        if !more && exhausted {
            self.state.completed_passes = self.state.completed_passes.saturating_add(1);
        }
        self.retrying = retryable || result.is_err();
        let delay = if retryable {
            self.backoff_failures = self.backoff_failures.saturating_add(1);
            let exponential = self.policy.initial_backoff_seconds
                * self
                    .policy
                    .backoff_multiplier
                    .powi(self.backoff_failures.min(1024) as i32 - 1);
            let cap = exponential.min(self.policy.backoff_cap_seconds);
            // UUID randomness avoids a shared retry wave without another RNG API.
            let unit = (Uuid::new_v4().as_u128() as u64 as f64) / (u64::MAX as f64);
            let backoff = Duration::from_secs_f64(cap * (0.5 + unit * 0.5));
            let delay = retry_error
                .as_ref()
                .and_then(|e| e.retry_after)
                .unwrap_or_default()
                .max(backoff);
            if exhausted {
                delay.max(Duration::from_secs_f64(self.policy.poll_seconds))
            } else {
                delay
            }
        } else if result.is_err() {
            Duration::from_secs_f64(self.policy.backoff_cap_seconds)
        } else if more {
            Duration::from_millis(1)
        } else {
            Duration::from_secs_f64(self.policy.poll_seconds)
        };
        if exhausted {
            self.attempt = 0;
        }
        // Never shorten Retry-After. An unrepresentable deadline disables
        // automatic polling until the session is disconnected/reconfigured;
        // commands and cached state remain available.
        self.next_poll = Instant::now().checked_add(delay);
        self.poll_reason = Some(if self.retrying {
            PollReason::Retry
        } else if more {
            PollReason::Continuation
        } else {
            PollReason::Periodic
        });
        self.publish();
        let _ = self.events.send(event);
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn validate_batch(state: &SourceSnapshot, batch: &SampleBatch) -> Result<(), SourceError> {
    let invalid = || {
        SourceError::new(
            ErrorKind::Permanent,
            "Invalid or oversized source sample cache",
        )
    };
    if batch.values.len() > MAX_SAMPLE_KEYS
        || batch.errors.len() > MAX_SAMPLE_KEYS
        || batch.ages_seconds.len() > MAX_SAMPLE_KEYS
    {
        return Err(invalid());
    }
    // Include historical sequence keys: changing key names must not accumulate
    // an unbounded counter map even when complete batches replace old values.
    let mut keys: BTreeSet<&str> = state.sample_sequences.keys().map(String::as_str).collect();
    if batch.partial {
        keys.extend(state.sample_errors.keys().map(String::as_str));
    }
    for key in batch.values.keys().chain(batch.errors.keys()) {
        if key.is_empty() || key.len() > 200 {
            return Err(invalid());
        }
        keys.insert(key);
    }
    if keys.len() > MAX_SAMPLE_KEYS
        || batch
            .values
            .keys()
            .any(|key| batch.errors.contains_key(key))
    {
        return Err(invalid());
    }
    for (key, age) in &batch.ages_seconds {
        if !batch.values.contains_key(key) || !age.is_finite() || *age < 0.0 {
            return Err(invalid());
        }
    }
    let mut budget = SampleBudget::default();
    let retained = state.values.iter().filter(|(key, _)| {
        batch.partial && !batch.values.contains_key(*key) && !batch.errors.contains_key(*key)
    });
    for (_, value) in retained.chain(batch.values.iter()) {
        if !budget.admit(value) {
            return Err(invalid());
        }
    }
    Ok(())
}
