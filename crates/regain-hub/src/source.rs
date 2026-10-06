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
pub type BackendFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SourceError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    Disconnected,
    Busy,
    Transient,
    Permanent,
    Uncertain,
    Unsupported,
    InvalidValue,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
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
    fn connect(&mut self) -> BackendFuture<'_, ()>;
    fn disconnect(&mut self) -> BackendFuture<'_, ()>;
    fn read(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value>;
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value>;
    fn poll(&mut self) -> BackendFuture<'_, Values>;
    fn reset(&mut self);
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSnapshot {
    pub source: Uuid,
    pub revision: Uuid,
    pub generation: Uuid,
    pub sequence: u64,
    pub transport_connected: bool,
    pub lease_count: usize,
    pub values: Values,
    pub sampled_at_seconds: Option<f64>,
    pub error: Option<SourceError>,
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
        member: String,
        parameters: Values,
        reply: Reply<Value>,
    },
    Write {
        lease: Uuid,
        member: String,
        parameters: Values,
        reply: Reply<Value>,
    },
}

pub struct SourceHandle {
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<SourceSnapshot>,
    events: broadcast::Sender<PollEvent>,
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
            lease_count: 0,
            values: Values::new(),
            sampled_at_seconds: None,
            error: None,
        };
        let (commands, receiver) = mpsc::channel(16);
        let (snapshot, reader) = watch::channel(initial.clone());
        let (events, _) = broadcast::channel(64);
        let handle = Arc::new(Self {
            commands,
            snapshot: reader,
            events: events.clone(),
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
            }
            .run(receiver),
        );
        Ok(handle)
    }
    pub fn snapshot(&self) -> SourceSnapshot {
        self.snapshot.borrow().clone()
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
    pub async fn read(
        &self,
        lease: Uuid,
        member: &str,
        parameters: Values,
    ) -> Result<Value, SourceError> {
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::Read {
            lease,
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
        let (reply, response) = oneshot::channel();
        self.enqueue(Command::Write {
            lease,
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
}
impl Actor {
    fn deadline(&self) -> Duration {
        Duration::from_secs_f64(self.policy.request_timeout_seconds)
    }
    fn publish(&mut self) {
        self.state.lease_count = self.leases.len();
        self.snapshot.send_replace(self.state.clone());
    }
    fn fault(&mut self, error: SourceError) {
        self.state.error = Some(error);
        self.state.transport_connected = false;
        self.state.generation = Uuid::new_v4();
        self.state.sequence = 0;
        self.state.values.clear();
        self.state.sampled_at_seconds = None;
        self.backend.reset();
        self.publish();
    }
    async fn connect(&mut self) -> Result<(), SourceError> {
        if self.state.transport_connected {
            return Ok(());
        }
        let result = timeout(self.deadline(), self.backend.connect())
            .await
            .unwrap_or_else(|_| Err(SourceError::timeout()));
        match result {
            Ok(()) => {
                self.state.transport_connected = true;
                // New transport cannot inherit another worker's safe evidence.
                self.state.generation = Uuid::new_v4();
                self.state.sequence = 0;
                self.state.values.clear();
                self.state.sampled_at_seconds = None;
                self.state.error = None;
                self.publish();
                Ok(())
            }
            Err(e) => {
                self.fault(e.clone());
                Err(e)
            }
        }
    }
    async fn disconnect(&mut self) {
        let result = timeout(self.deadline(), self.backend.disconnect()).await;
        if !matches!(result, Ok(Ok(()))) {
            self.backend.reset();
        }
        self.state.transport_connected = false;
        self.state.values.clear();
        self.state.sampled_at_seconds = None;
        self.state.generation = Uuid::new_v4();
        self.state.sequence = 0;
        self.controller = None;
        self.write_uncertain = false;
        self.attempt = 0;
        self.backoff_failures = 0;
        self.publish();
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
                command=commands.recv() => match command { Some(command)=>self.command(command).await, None=>break }
            }
        }
        self.leases.clear();
        self.disconnect().await;
        self.state.error = Some(closed());
        self.publish();
    }
    async fn command(&mut self, command: Command) {
        match command {
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
                    let delay = match self.connect().await {
                        Ok(()) => Duration::ZERO,
                        Err(error) => {
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
                    };
                    self.next_poll = Instant::now().checked_add(delay);
                }
                // Offline sources retain their lease and are polled for recovery.
                // A SafetyMonitor output can remain connected while unsafe.
                self.publish();
                if reply.send(Ok(self.state.clone())).is_err() && inserted {
                    self.leases.remove(&lease);
                    if self.leases.is_empty() {
                        self.disconnect().await;
                    } else {
                        self.publish();
                    }
                }
            }
            Command::Release { lease, reply } => {
                if self.leases.remove(&lease) && self.leases.is_empty() {
                    self.disconnect().await;
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
            Command::Read {
                lease,
                member,
                parameters,
                reply,
            } => {
                if reply.is_closed() {
                    return;
                }
                let result = match self.authorized(lease, false) {
                    Err(e) => Err(e),
                    Ok(()) => match self.connect().await {
                        Err(e) => Err(e),
                        Ok(()) => timeout(self.deadline(), self.backend.read(member, parameters))
                            .await
                            .unwrap_or_else(|_| Err(SourceError::timeout())),
                    },
                };
                if let Err(e) = &result
                    && e.transport_lost
                {
                    self.fault(e.clone());
                }
                let _ = reply.send(result);
            }
            Command::Write {
                lease,
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
                    && error.transport_lost
                {
                    // A definitive rejection can retire the transport without
                    // making the command ambiguous.
                    self.fault(error.clone());
                }
                let _ = reply.send(result);
            }
        }
    }
    async fn poll(&mut self) {
        let started = self.clock.now();
        self.attempt += 1;
        let result = match self.connect().await {
            Err(e) => Err(e),
            Ok(()) => timeout(self.deadline(), self.backend.poll())
                .await
                .unwrap_or_else(|_| Err(SourceError::timeout())),
        };
        let received = self.clock.now();
        let retryable = result
            .as_ref()
            .is_err_and(|e| matches!(e.kind, ErrorKind::Transient | ErrorKind::Disconnected));
        let exhausted = !retryable || self.attempt >= self.policy.attempts_per_cycle;
        if let Err(error) = &result
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
            started,
            received,
            result: result.clone(),
            cycle_exhausted: exhausted,
        };
        match &result {
            Ok(values) => {
                self.state.values = values.clone();
                self.state.sampled_at_seconds = Some(started.as_secs_f64());
                self.state.error = None;
                self.backoff_failures = 0;
            }
            Err(error) => {
                self.state.error = Some(error.clone());
            }
        }
        self.publish();
        let _ = self.events.send(event);
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
            let delay = result
                .as_ref()
                .err()
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
    }
}

async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
