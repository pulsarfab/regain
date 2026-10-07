//! One retained core camera owner per source. Frontend waiters never own its tasks.
//! Client authorization and source leases remain in the outer CameraSupervisor.
use super::{
    image::{CameraImage, ImageBudget},
    native_capture::{NativeCaptureError, capture_admitted},
    native_properties::{
        NativeGeometry, NativeProperties, NativePropertyObservation, validate_control_request,
        validate_imaging_control,
    },
    properties::{CameraProperty, CameraSetting, CameraValue},
};
use crate::{
    activity::{Activity, ActivityCounter},
    source::{ErrorKind, SourceError},
};
use regain_core::{
    CancellationToken, Diagnostic, Exposure, Runtime, Selection, Session, SharedStatus, Status,
    cooling::{CoolingCancellation, CoolingError, CoolingHandle, CoolingReceipt},
};
use serde::Serialize;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeOperationKind {
    Connecting,
    Capturing,
    Configuring,
    Refreshing,
    Aborting,
    Closing,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeOperation {
    pub id: Uuid,
    pub kind: NativeOperationKind,
    pub exposure: Option<Exposure>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeCoolingOperation {
    pub id: Uuid,
    pub control: i32,
    pub value: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeCameraSnapshot {
    pub generation: Uuid,
    pub connected: bool,
    pub operation: Option<NativeOperation>,
    pub cooling: Option<NativeCoolingOperation>,
    pub geometry: Option<NativeGeometry>,
    pub acquisition: Option<Uuid>,
    pub image_ready: bool,
    pub error: Option<SourceError>,
    pub core: Status,
}
struct Pending {
    operation: NativeOperation,
    token: CancellationToken,
}
struct Completed {
    id: Uuid,
    image: CameraImage,
}
struct PendingCooling {
    operation: NativeCoolingOperation,
    token: CancellationToken,
    cancellation: CoolingCancellation,
}
struct State {
    generation: Uuid,
    connected: bool,
    pending: Option<Pending>,
    cooling: Option<PendingCooling>,
    geometry: Option<NativeGeometry>,
    completed: Option<Completed>,
    last_acquisition: Option<Uuid>,
    error: Option<SourceError>,
}
impl State {
    fn publication_pending(&self) -> bool {
        self.cooling.is_some()
            || self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.operation.kind != NativeOperationKind::Refreshing)
    }
}
/// Constructed by the native source adapter, sharing the host's budget/activity.
/// This type deliberately exposes no client connection lease or automatic retries.
pub struct NativeCamera {
    engine: AsyncMutex<Session>,
    status: SharedStatus,
    state: Mutex<State>,
    budget: ImageBudget,
    activity: ActivityCounter,
    // Per-owner retirement includes obsolete generations, without waiting for
    // unrelated cameras/outputs using the host-wide activity counter.
    retained: ActivityCounter,
    changed: Notify,
    sdk_fallback: bool,
    simulated: bool,
    cooling: CoolingHandle,
    command_timeout: Duration,
}

// The caller only withdraws work that has not been dispatched. Its receipt and
// activity are retained by CoolingWork even when this guard is dropped.
struct CoolingWaiter(CoolingCancellation);
impl Drop for CoolingWaiter {
    fn drop(&mut self) {
        self.0.cancel_before_dispatch();
    }
}
struct CoolingWork {
    owner: Arc<NativeCamera>,
    generation: Uuid,
    id: Uuid,
    _activity: RetainedTask,
}
impl Drop for CoolingWork {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().unwrap();
        if state.generation == self.generation
            && state
                .cooling
                .as_ref()
                .is_some_and(|p| p.operation.id == self.id)
        {
            let pending = state.cooling.take().unwrap();
            pending.cancellation.cancel_before_dispatch();
            pending.token.cancel();
            if let Some(capture) = &state.pending {
                capture.token.cancel();
            }
            state.error = Some(SourceError::new(
                ErrorKind::Uncertain,
                "Native cooler task stopped before its outcome was known",
            ));
        }
        self.owner.changed.notify_waiters();
    }
}

// Unexpected task loss must not leave a permanent Busy marker or claim a usable
// worker. A subsequent explicit connection closes the old session before opening.
struct Work {
    owner: Arc<NativeCamera>,
    generation: Uuid,
    id: Uuid,
    _activity: RetainedTask,
}
// Reserve before spawning, including adapter connection waiters that may not
// have entered the core owner yet. Global activity drops before local retirement
// so observing this owner drained also observes its host activity released.
pub(crate) struct RetainedTask {
    _activity: Activity,
    _retained: Activity,
}
impl Drop for Work {
    fn drop(&mut self) {
        let mut state = self.owner.state.lock().unwrap();
        if state.generation == self.generation
            && state
                .pending
                .as_ref()
                .is_some_and(|p| p.operation.id == self.id)
        {
            let configuring = state
                .pending
                .as_ref()
                .is_some_and(|p| p.operation.kind == NativeOperationKind::Configuring);
            state.pending = None;
            state.connected = false;
            if !state
                .error
                .as_ref()
                .is_some_and(|e| e.kind == ErrorKind::Uncertain)
            {
                state.error = Some(SourceError::new(
                    if configuring {
                        ErrorKind::Uncertain
                    } else {
                        ErrorKind::Unavailable
                    },
                    "Native camera task stopped before completion",
                ));
            }
        }
        self.owner.changed.notify_waiters();
    }
}

fn busy() -> SourceError {
    SourceError::new(ErrorKind::Busy, "Native camera operation is already active")
}
fn disconnected() -> SourceError {
    SourceError::new(
        ErrorKind::Disconnected,
        "Native camera generation changed or is disconnected",
    )
}
fn core_error(error: &anyhow::Error) -> SourceError {
    if matches!(
        error.downcast_ref::<CoolingError>(),
        Some(CoolingError::Expired)
    ) {
        return SourceError::new(
            ErrorKind::Transient,
            "Native command expired before dispatch",
        );
    }
    let kind = match error.downcast_ref::<regain_core::Failure>() {
        Some(regain_core::Failure::Invalid(_)) => ErrorKind::InvalidValue,
        Some(regain_core::Failure::Cancelled) => ErrorKind::Unavailable,
        Some(regain_core::Failure::UncertainControl { .. }) => ErrorKind::Uncertain,
        _ => ErrorKind::Unavailable,
    };
    let mut result = SourceError::new(
        kind,
        "Native camera operation failed; inspect its core diagnostics",
    );
    if let Some(
        regain_core::Failure::Worker { code, .. }
        | regain_core::Failure::UncertainControl { code, .. },
    ) = error.downcast_ref::<regain_core::Failure>()
    {
        result.upstream_code = *code;
    }
    result
}
fn capture_error(error: NativeCaptureError) -> SourceError {
    match error {
        NativeCaptureError::Contract(error) => error,
        NativeCaptureError::Capture(error) => core_error(&error),
    }
}
fn cooling_error(error: CoolingError) -> SourceError {
    let (kind, message) = match &error {
        CoolingError::Busy => (ErrorKind::Busy, "Native cooler command is already active"),
        CoolingError::Unavailable => (ErrorKind::Unavailable, "Native cooler is unavailable"),
        CoolingError::Expired => (
            ErrorKind::Transient,
            "Native cooler command expired before dispatch",
        ),
        CoolingError::Cancelled => (
            ErrorKind::Unavailable,
            "Native cooler command was cancelled before dispatch",
        ),
        CoolingError::Invalid(_) => (
            ErrorKind::InvalidValue,
            "Native cooler control or value is invalid",
        ),
        CoolingError::Uncertain { .. } => (
            ErrorKind::Uncertain,
            "Native cooler outcome is uncertain; reconnect explicitly",
        ),
    };
    let mut result = SourceError::new(kind, message);
    if let CoolingError::Uncertain { code, .. } = error {
        result.upstream_code = code;
    }
    result
}

impl NativeCamera {
    pub fn simulated(&self) -> bool {
        self.simulated
    }
    pub(crate) fn shares_budget(&self, budget: &ImageBudget) -> bool {
        self.budget.shares(budget)
    }
    /// A known explicit control request can restore the worker retired by Abort.
    /// Restoration only reapplies acknowledged settings; the requested new value
    /// is dispatched by its normal retained command after this method succeeds.
    pub(crate) async fn prepare_control(
        self: &Arc<Self>,
        control: i32,
        value: i64,
    ) -> Result<(), SourceError> {
        let (generation, id) = {
            let mut state = self.state.lock().unwrap();
            if !state.connected {
                return Err(disconnected());
            }
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            let core = self.status.lock().unwrap();
            validate_control_request(&core, control, value)?;
            if core.control_connection_available {
                return Ok(());
            }
            if state.pending.is_some() || state.cooling.is_some() {
                return Err(busy());
            }
            drop(core);
            let generation = state.generation;
            let id = Uuid::new_v4();
            let token = CancellationToken::new();
            let work = self.work(generation, id);
            state.pending = Some(Pending {
                operation: NativeOperation {
                    id,
                    kind: NativeOperationKind::Configuring,
                    exposure: None,
                },
                token: token.clone(),
            });
            tokio::spawn(async move {
                let owner = &work.owner;
                let mut session = owner.engine.lock().await;
                if !owner.current(generation, id) {
                    return;
                }
                let result = session.refresh(&token).await.map_err(|e| core_error(&e));
                let mut state = owner.state.lock().unwrap();
                if state.generation == generation
                    && state.pending.as_ref().is_some_and(|p| p.operation.id == id)
                {
                    state.pending = None;
                    if let Err(error) = result {
                        state.connected = false;
                        state.error = Some(error);
                    }
                }
                owner.changed.notify_waiters();
            });
            (generation, id)
        };
        self.wait_operation(generation, id).await?;
        let state = self.state.lock().unwrap();
        if state.generation != generation {
            return Err(disconnected());
        }
        state.error.clone().map_or(Ok(()), Err)
    }
    /// An adapter command can await this read-only reservation before dispatch.
    /// Cancellation withdraws only the caller, not the retained telemetry task.
    pub(crate) async fn wait_environment_refresh(&self) -> Result<(), SourceError> {
        let pending = {
            let state = self.state.lock().unwrap();
            state
                .pending
                .as_ref()
                .filter(|p| p.operation.kind == NativeOperationKind::Refreshing)
                .map(|p| (state.generation, p.operation.id))
        };
        if let Some((generation, id)) = pending {
            self.wait_operation(generation, id).await?;
        }
        Ok(())
    }
    /// Schedule read-only telemetry without lending task ownership to a poll
    /// waiter. Capture and acknowledged settings already own the same engine;
    /// a background poll never queues behind them or opens another worker.
    pub fn begin_environment_refresh(self: &Arc<Self>) -> Result<bool, SourceError> {
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            return Err(disconnected());
        }
        if let Some(error) = state
            .error
            .as_ref()
            .filter(|e| e.kind == ErrorKind::Uncertain)
        {
            return Err(error.clone());
        }
        if state.pending.is_some() || state.cooling.is_some() {
            return Ok(false);
        }
        if !self.status.lock().unwrap().control_connection_available {
            // No implicit reopen after a capture/Abort retired the worker.
            // Cached readings retain their ages until explicit work restores it.
            return Ok(false);
        }
        let generation = state.generation;
        let id = Uuid::new_v4();
        let token = CancellationToken::new();
        let work = self.work(generation, id);
        state.pending = Some(Pending {
            operation: NativeOperation {
                id,
                kind: NativeOperationKind::Refreshing,
                exposure: None,
            },
            token: token.clone(),
        });
        tokio::spawn(async move {
            let owner = &work.owner;
            let mut session = owner.engine.lock().await;
            if !owner.current(generation, id) {
                return;
            }
            let result = session
                .refresh_environment(&token)
                .await
                .map_err(|e| core_error(&e));
            let mut state = owner.state.lock().unwrap();
            if state.generation == generation
                && state.pending.as_ref().is_some_and(|p| p.operation.id == id)
            {
                state.pending = None;
                if let Err(mut error) = result {
                    error.transport_lost = true;
                    state.error = Some(error);
                    state.connected = false;
                }
            }
            owner.changed.notify_waiters();
        });
        Ok(true)
    }
    /// No discovery or I/O. Simulation is explicitly selected by Runtime.
    pub fn new(
        selection: Selection,
        runtime: Runtime,
        budget: ImageBudget,
        activity: ActivityCounter,
        log: Diagnostic,
    ) -> Result<Arc<Self>, SourceError> {
        let sdk_fallback = selection.direct && selection.sdk_fallback;
        let simulated = runtime.simulate;
        let session = Session::new(selection, runtime, log).map_err(|e| core_error(&e))?;
        let command_timeout =
            Duration::from_secs_f64(session.selection.recovery.command_timeout_seconds);
        Ok(Arc::new(Self {
            status: session.status.clone(),
            cooling: session.cooling(),
            command_timeout,
            engine: AsyncMutex::new(session),
            state: Mutex::new(State {
                generation: Uuid::new_v4(),
                connected: false,
                pending: None,
                cooling: None,
                geometry: None,
                completed: None,
                last_acquisition: None,
                error: None,
            }),
            budget,
            activity,
            retained: ActivityCounter::default(),
            changed: Notify::new(),
            sdk_fallback,
            simulated,
        }))
    }
    pub fn snapshot(&self) -> NativeCameraSnapshot {
        let state = self.state.lock().unwrap();
        NativeCameraSnapshot {
            generation: state.generation,
            connected: state.connected,
            operation: state.pending.as_ref().map(|p| p.operation.clone()),
            cooling: state.cooling.as_ref().map(|p| p.operation.clone()),
            geometry: state.geometry,
            acquisition: state.last_acquisition,
            image_ready: state.connected
                && !state.publication_pending()
                && state.completed.is_some()
                && state.error.is_none(),
            error: state.error.clone(),
            core: self.status.lock().unwrap().clone(),
        }
    }
    fn work(self: &Arc<Self>, generation: Uuid, id: Uuid) -> Work {
        Work {
            owner: self.clone(),
            generation,
            id,
            _activity: self.retain_task(),
        }
    }
    pub(crate) fn retain_task(&self) -> RetainedTask {
        RetainedTask {
            _activity: Activity::new(self.activity.clone()),
            _retained: Activity::new(self.retained.clone()),
        }
    }
    fn current(&self, generation: Uuid, id: Uuid) -> bool {
        let state = self.state.lock().unwrap();
        state.generation == generation
            && state.pending.as_ref().is_some_and(|p| p.operation.id == id)
    }
    /// Once polled, the handshake is retained even if its caller stops waiting.
    /// Concurrent callers join the same connection rather than opening twice.
    pub async fn connect(self: &Arc<Self>) -> Result<(), SourceError> {
        let (generation, id) = {
            let mut state = self.state.lock().unwrap();
            if let Some(error) = state
                .error
                .as_ref()
                .filter(|e| e.kind == ErrorKind::Uncertain)
            {
                return Err(error.clone());
            }
            if state.connected {
                return Ok(());
            }
            if state.cooling.is_some() {
                return Err(busy());
            }
            if let Some(pending) = &state.pending {
                if pending.operation.kind != NativeOperationKind::Connecting {
                    return Err(busy());
                }
                (state.generation, pending.operation.id)
            } else {
                let generation = Uuid::new_v4();
                let id = Uuid::new_v4();
                let token = CancellationToken::new();
                let work = self.work(generation, id);
                state.generation = generation;
                state.error = None;
                state.completed = None;
                state.geometry = None;
                state.last_acquisition = None;
                state.pending = Some(Pending {
                    operation: NativeOperation {
                        id,
                        kind: NativeOperationKind::Connecting,
                        exposure: None,
                    },
                    token: token.clone(),
                });
                tokio::spawn(async move {
                    let owner = &work.owner;
                    let mut session = owner.engine.lock().await;
                    if !owner.current(generation, id) {
                        return;
                    }
                    session.close().await;
                    if !owner.current(generation, id) {
                        return;
                    }
                    let result = async {
                        session.connect(&token).await.map_err(|e| core_error(&e))?;
                        let geometry = NativeGeometry::initial(&session.snapshot().info)?;
                        // Publish acknowledged initial controls/environment, not
                        // the worker's desired/default values from its open reply.
                        session.refresh(&token).await.map_err(|e| core_error(&e))?;
                        Ok::<_, SourceError>(geometry)
                    }
                    .await;
                    if result.is_err() {
                        session.close().await;
                    }
                    if !owner.current(generation, id) {
                        session.close().await;
                        return;
                    }
                    {
                        let mut state = owner.state.lock().unwrap();
                        if state.generation == generation
                            && state.pending.as_ref().is_some_and(|p| p.operation.id == id)
                        {
                            state.pending = None;
                            state.connected = result.is_ok();
                            match result {
                                Ok(geometry) => state.geometry = Some(geometry),
                                Err(error) => state.error = Some(error),
                            }
                        }
                    }
                    owner.changed.notify_waiters();
                });
                (generation, id)
            }
        };
        self.wait_operation(generation, id).await?;
        let state = self.state.lock().unwrap();
        if state.generation != generation {
            return Err(disconnected());
        }
        if state.connected {
            Ok(())
        } else {
            Err(state.error.clone().unwrap_or_else(disconnected))
        }
    }
    async fn wait_operation(&self, generation: Uuid, id: Uuid) -> Result<(), SourceError> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                if state.generation != generation {
                    return Err(disconnected());
                }
                if state.pending.as_ref().is_none_or(|p| p.operation.id != id) {
                    return Ok(());
                }
            }
            notified.await;
        }
    }
    /// Admission and task ownership are atomic. Invalid/busy requests preserve
    /// an older completed image; a successful start clears it before returning.
    pub fn start(self: &Arc<Self>, exposure: Exposure) -> Result<Uuid, SourceError> {
        let mut state = self.state.lock().unwrap();
        self.start_locked(&mut state, exposure)
    }
    /// Freeze the currently configured ROI atomically with capture admission.
    pub fn start_configured(
        self: &Arc<Self>,
        microseconds: u64,
        dark: bool,
    ) -> Result<Uuid, SourceError> {
        let mut state = self.state.lock().unwrap();
        let exposure = state
            .geometry
            .ok_or_else(disconnected)?
            .exposure(microseconds, dark);
        self.start_locked(&mut state, exposure)
    }
    fn start_locked(
        self: &Arc<Self>,
        state: &mut State,
        exposure: Exposure,
    ) -> Result<Uuid, SourceError> {
        if !state.connected {
            return Err(disconnected());
        }
        if let Some(error) = state
            .error
            .as_ref()
            .filter(|e| e.kind == ErrorKind::Uncertain)
        {
            return Err(error.clone());
        }
        if state.pending.is_some() || state.cooling.is_some() {
            return Err(busy());
        }
        let core = self.status.lock().unwrap().clone();
        regain_core::validate_capture(&core.info, &core.controls, &exposure, self.sdk_fallback)
            .map_err(|e| core_error(&e))?;
        let permit = self.budget.reserve_native(&exposure)?;
        let id = Uuid::new_v4();
        let generation = state.generation;
        let token = CancellationToken::new();
        let work = self.work(generation, id);
        state.geometry = Some(NativeGeometry::from_exposure(&exposure));
        state.pending = Some(Pending {
            operation: NativeOperation {
                id,
                kind: NativeOperationKind::Capturing,
                exposure: Some(exposure.clone()),
            },
            token: token.clone(),
        });
        state.completed = None;
        state.last_acquisition = Some(id);
        state.error = None;
        tokio::spawn(async move {
            let owner = &work.owner;
            let mut session = owner.engine.lock().await;
            if !owner.current(generation, id) {
                return;
            }
            let result = capture_admitted(&mut session, exposure, permit, &token)
                .await
                .map_err(capture_error);
            if !owner.current(generation, id) {
                session.close().await;
                return;
            }
            {
                let mut state = owner.state.lock().unwrap();
                if state.generation == generation
                    && state.pending.as_ref().is_some_and(|p| p.operation.id == id)
                {
                    state.pending = None;
                    if !token.is_cancelled()
                        && !state
                            .error
                            .as_ref()
                            .is_some_and(|e| e.kind == ErrorKind::Uncertain)
                    {
                        match result {
                            Ok(image) => state.completed = Some(Completed { id, image }),
                            Err(error) => state.error = Some(error),
                        }
                    }
                }
            }
            owner.changed.notify_waiters();
        });
        Ok(id)
    }
    /// Cached native properties remain readable while the retained core owner
    /// captures. The source adapter must preserve observation freshness.
    pub fn read_property(&self, property: CameraProperty) -> Result<CameraValue, SourceError> {
        self.read_property_observation(property)
            .map(|observed| observed.value)
    }
    /// Reading cached evidence never resets its age. Metadata/local state have
    /// no hardware observation time; adapters must preserve this distinction.
    pub fn read_property_observation(
        &self,
        property: CameraProperty,
    ) -> Result<NativePropertyObservation, SourceError> {
        let state = self.state.lock().unwrap();
        if !state.connected {
            return Err(disconnected());
        }
        let core = self.status.lock().unwrap().clone();
        NativeProperties {
            core: &core,
            geometry: state.geometry.ok_or_else(disconnected)?,
            operation: state.pending.as_ref().map(|p| p.operation.kind),
            image: state.completed.as_ref().map(|p| &p.image),
            image_ready: !state.publication_pending()
                && state.completed.is_some()
                && state.error.is_none(),
            error: state.error.as_ref(),
        }
        .observation(property)
    }
    /// Local bin/ROI/RAW16 selection only. Hardware settings must use the
    /// acknowledged core command path, never a desired-state queue as an ACK.
    pub fn configure_geometry(&self, setting: CameraSetting) -> Result<(), SourceError> {
        let mut state = self.state.lock().unwrap();
        if !state.connected {
            return Err(disconnected());
        }
        if let Some(error) = state
            .error
            .as_ref()
            .filter(|e| e.kind == ErrorKind::Uncertain)
        {
            return Err(error.clone());
        }
        if state.pending.is_some() || state.cooling.is_some() {
            return Err(busy());
        }
        let info = self.status.lock().unwrap().info.clone();
        let geometry = state
            .geometry
            .ok_or_else(disconnected)?
            .configured(setting, &info)?;
        state.geometry = Some(geometry);
        Ok(())
    }
    /// Retain an idle gain/offset write and its readback independently of callers.
    /// Caller loss before engine admission skips I/O; after admission the task
    /// owns dispatch through its bounded outcome and cleanup.
    pub async fn set_imaging_control(
        self: &Arc<Self>,
        control: i32,
        value: i64,
    ) -> Result<i64, SourceError> {
        let (generation, response) = {
            let mut state = self.state.lock().unwrap();
            if !state.connected {
                return Err(disconnected());
            }
            if let Some(error) = state
                .error
                .as_ref()
                .filter(|e| e.kind == ErrorKind::Uncertain)
            {
                return Err(error.clone());
            }
            if state.pending.is_some() || state.cooling.is_some() {
                return Err(busy());
            }
            validate_imaging_control(&self.status.lock().unwrap(), control, value)?;
            let generation = state.generation;
            let id = Uuid::new_v4();
            let token = CancellationToken::new();
            let work = self.work(generation, id);
            let deadline = tokio::time::Instant::now() + self.command_timeout;
            state.pending = Some(Pending {
                operation: NativeOperation {
                    id,
                    kind: NativeOperationKind::Configuring,
                    exposure: None,
                },
                token: token.clone(),
            });
            let (mut reply, response) = oneshot::channel();
            tokio::spawn(async move {
                let owner = &work.owner;
                let result = tokio::select! {
                    biased;
                    _ = reply.closed() => Err(SourceError::new(ErrorKind::Unavailable, "Native setting caller left before dispatch")),
                    _ = token.cancelled() => Err(disconnected()),
                    _ = tokio::time::sleep_until(deadline) => Err(SourceError::new(ErrorKind::Transient, "Native setting expired before dispatch")),
                    mut session = owner.engine.lock() => {
                        if reply.is_closed() {
                            Err(SourceError::new(ErrorKind::Unavailable, "Native setting caller left before dispatch"))
                        } else if !owner.current(generation, id) {
                            Err(disconnected())
                        } else {
                            // Do not select against caller loss after admission:
                            // the task retains write/readback/retirement ownership.
                            session.set_imaging_control(control, value, deadline, &token).await.map_err(|e| core_error(&e))
                        }
                    }
                };
                let result = {
                    let mut state = owner.state.lock().unwrap();
                    if state.generation != generation
                        || !state.pending.as_ref().is_some_and(|p| p.operation.id == id)
                    {
                        Err(disconnected())
                    } else {
                        state.pending = None;
                        if let Err(error) = &result {
                            if error.kind == ErrorKind::Uncertain {
                                state.error = Some(error.clone());
                            } else if !owner.status.lock().unwrap().control_connection_available {
                                state.connected = false;
                                state.error = Some(error.clone());
                            }
                        }
                        result
                    }
                };
                owner.changed.notify_waiters();
                drop(work);
                let _ = reply.send(result);
            });
            (generation, response)
        };
        let result = response.await.map_err(|_| {
            SourceError::new(
                ErrorKind::Uncertain,
                "Native setting task stopped before acknowledgement",
            )
        })?;
        let state = self.state.lock().unwrap();
        if state.generation != generation || !state.connected {
            return Err(disconnected());
        }
        result
    }
    /// The outer supervisor authorizes the source/capture owner. This method
    /// retains one acknowledged target/enable command through caller loss.
    pub async fn set_cooling(
        self: &Arc<Self>,
        control: i32,
        value: i64,
    ) -> Result<i64, SourceError> {
        let (generation, response, _waiter) = {
            let mut state = self.state.lock().unwrap();
            if !state.connected {
                return Err(disconnected());
            }
            if let Some(error) = state
                .error
                .as_ref()
                .filter(|e| e.kind == ErrorKind::Uncertain)
            {
                return Err(error.clone());
            }
            if state.cooling.is_some()
                || state
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.operation.kind != NativeOperationKind::Capturing)
            {
                return Err(busy());
            }
            // Reserve activity before exposing either core or source markers.
            let activity = self.retain_task();
            let receipt = self
                .cooling
                .submit(control, value, self.command_timeout)
                .map_err(cooling_error)?;
            let cancellation = receipt.cancellation();
            let waiter = CoolingWaiter(cancellation.clone());
            let id = Uuid::new_v4();
            let generation = state.generation;
            let token = CancellationToken::new();
            let work = CoolingWork {
                owner: self.clone(),
                generation,
                id,
                _activity: activity,
            };
            state.cooling = Some(PendingCooling {
                operation: NativeCoolingOperation { id, control, value },
                token: token.clone(),
                cancellation,
            });
            let (reply, response) = oneshot::channel();
            tokio::spawn(async move {
                let result = work
                    .owner
                    .drive_cooling(generation, id, receipt, &token)
                    .await;
                let result = {
                    let mut state = work.owner.state.lock().unwrap();
                    if state.generation != generation
                        || !state.cooling.as_ref().is_some_and(|p| p.operation.id == id)
                    {
                        Err(disconnected())
                    } else {
                        state.cooling = None;
                        if let Err(error) = &result
                            && error.kind == ErrorKind::Uncertain
                        {
                            state.error = Some(error.clone());
                            // Do not let the capture publish over this fence.
                            if let Some(capture) = &state.pending {
                                capture.token.cancel();
                            }
                        }
                        result
                    }
                };
                work.owner.changed.notify_waiters();
                // Activity is released before acknowledgement, after the owner
                // has finished service/cleanup and cleared its marker.
                drop(work);
                let _ = reply.send(result);
            });
            (generation, response, waiter)
        };
        let result = response.await.map_err(|_| {
            SourceError::new(
                ErrorKind::Uncertain,
                "Native cooler task stopped before acknowledgement",
            )
        })?;
        let state = self.state.lock().unwrap();
        if state.generation != generation || !state.connected {
            return Err(disconnected());
        }
        result
    }
    async fn drive_cooling(
        &self,
        generation: Uuid,
        id: Uuid,
        receipt: CoolingReceipt,
        token: &CancellationToken,
    ) -> Result<i64, SourceError> {
        let cancellation = receipt.cancellation();
        let outcome = receipt.wait();
        tokio::pin!(outcome);
        // Capture owns the engine while exposing/downloading and services this
        // mailbox itself. Wait for either its receipt or idle engine access.
        let result = tokio::select! {
            biased;
            result = &mut outcome => result,
            mut session = self.engine.lock() => {
                let current = {
                    let state = self.state.lock().unwrap();
                    state.connected && state.generation == generation
                        && !state.error.as_ref().is_some_and(|e| e.kind == ErrorKind::Uncertain)
                        && state.cooling.as_ref().is_some_and(|p| p.operation.id == id)
                };
                if current {
                    // Never select/drop a dispatched service future: retain it
                    // through worker retirement even if the receipt expires.
                    let _ = session.service_cooling(token).await;
                } else {
                    cancellation.cancel_before_dispatch();
                }
                outcome.await
            }
        };
        result.map_err(cooling_error)
    }
    pub fn image(&self) -> Result<CameraImage, SourceError> {
        let state = self.state.lock().unwrap();
        if !state.connected {
            return Err(disconnected());
        }
        if state.publication_pending() {
            return Err(busy());
        }
        if let Some(error) = &state.error {
            return Err(error.clone());
        }
        state
            .completed
            .as_ref()
            .map(|frame| frame.image.clone())
            .ok_or_else(|| {
                SourceError::new(
                    ErrorKind::Unavailable,
                    "No native camera image is available",
                )
            })
    }
    /// Waiting owns no exposure or cancellation token. Dropping this future is inert.
    pub async fn wait(&self, acquisition: Uuid) -> Result<CameraImage, SourceError> {
        let generation = self.state.lock().unwrap().generation;
        self.wait_operation(generation, acquisition).await?;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.state.lock().unwrap();
                if state.generation != generation {
                    return Err(disconnected());
                }
                if state.last_acquisition != Some(acquisition) {
                    return Err(SourceError::new(
                        ErrorKind::Unavailable,
                        "Native acquisition is no longer current",
                    ));
                }
                if !state.publication_pending() {
                    if let Some(error) = &state.error {
                        return Err(error.clone());
                    }
                    return state
                        .completed
                        .as_ref()
                        .filter(|frame| frame.id == acquisition)
                        .map(|frame| frame.image.clone())
                        .ok_or_else(|| {
                            SourceError::new(
                                ErrorKind::Unavailable,
                                "Native acquisition has no published image",
                            )
                        });
                }
            }
            notified.await;
        }
    }
    /// Explicit owner command only. It acknowledges after core cleanup, discards
    /// the active result and never implements StopExposure as AbortExposure.
    pub async fn abort(&self) -> Result<(), SourceError> {
        let (generation, id) = {
            let mut state = self.state.lock().unwrap();
            if !state.connected {
                return Err(disconnected());
            }
            if let Some(error) = state
                .error
                .as_ref()
                .filter(|e| e.kind == ErrorKind::Uncertain)
            {
                return Err(error.clone());
            }
            if state.cooling.is_some() {
                return Err(busy());
            }
            let generation = state.generation;
            let Some(pending) = state.pending.as_mut() else {
                return Ok(());
            };
            if !matches!(
                pending.operation.kind,
                NativeOperationKind::Capturing | NativeOperationKind::Aborting
            ) {
                return Err(busy());
            }
            pending.operation.kind = NativeOperationKind::Aborting;
            pending.token.cancel();
            (generation, pending.operation.id)
        };
        self.wait_operation(generation, id).await?;
        let state = self.state.lock().unwrap();
        if state.generation != generation || !state.connected {
            return Err(disconnected());
        }
        state.error.clone().map_or(Ok(()), Err)
    }
    /// Source teardown only; frontend connection leases must not call this directly.
    pub async fn close(self: &Arc<Self>) -> Result<(), SourceError> {
        let work = self.retire();
        let generation = work.generation;
        let id = work.id;
        self.spawn_close(work);
        self.wait_operation(generation, id).await?;
        let state = self.state.lock().unwrap();
        if state.generation != generation {
            return Err(disconnected());
        }
        state.error.clone().map_or(Ok(()), Err)
    }
    /// Fence synchronously, cancel retained core work and close its worker in an
    /// owned task. A later connect cannot be closed by this retired generation.
    pub fn reset(self: &Arc<Self>) {
        let work = self.retire();
        self.spawn_close(work);
    }
    /// Called after the source command queue is closed and disconnect/reset has
    /// fenced admission. Join all retained work, including retired generations;
    /// a caller deadline or a cleared pending marker is not worker retirement.
    pub(crate) async fn finish_shutdown(&self) {
        self.retained.wait_idle().await;
    }
    fn retire(self: &Arc<Self>) -> Work {
        let generation = Uuid::new_v4();
        let id = Uuid::new_v4();
        let work = self.work(generation, id);
        let mut state = self.state.lock().unwrap();
        if let Some(pending) = state.pending.take() {
            pending.token.cancel();
        }
        if let Some(cooling) = state.cooling.take() {
            cooling.cancellation.cancel_before_dispatch();
            cooling.token.cancel();
        }
        state.generation = generation;
        state.connected = false;
        state.completed = None;
        state.geometry = None;
        state.last_acquisition = None;
        state.error = None;
        state.pending = Some(Pending {
            operation: NativeOperation {
                id,
                kind: NativeOperationKind::Closing,
                exposure: None,
            },
            token: CancellationToken::new(),
        });
        self.changed.notify_waiters();
        work
    }
    fn spawn_close(self: &Arc<Self>, work: Work) {
        let generation = work.generation;
        let id = work.id;
        tokio::spawn(async move {
            let owner = &work.owner;
            let mut session = owner.engine.lock().await;
            if !owner.current(generation, id) {
                return;
            }
            session.close().await;
            let mut state = owner.state.lock().unwrap();
            if state.generation == generation
                && state.pending.as_ref().is_some_and(|p| p.operation.id == id)
            {
                state.pending = None;
            }
            owner.changed.notify_waiters();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regain_core::RecoveryOptions;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn simulated() -> Arc<NativeCamera> {
        simulated_with_log(Arc::new(|_, _, _| {}))
    }
    fn simulated_with_log(log: Diagnostic) -> Arc<NativeCamera> {
        NativeCamera::new(
            Selection {
                name: "ZWO Simulated".into(),
                serial: None,
                direct: false,
                sdk_fallback: false,
                recovery: RecoveryOptions::default(),
            },
            Runtime {
                directory: std::env::var_os("REGAIN_TEST_WORKERS")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
                    }),
                sdk: "unused".into(),
                simulate: true,
                sdk_simulation: Some(json!({"instant":true})),
            },
            ImageBudget::new(1024 * 1024).unwrap(),
            ActivityCounter::default(),
            log,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn source_shutdown_joins_retired_generations_and_keeps_disconnect_uncertainty() {
        use crate::{
            camera::native_source::NativeCameraBackend,
            config::HubConfig,
            endpoint::Endpoint,
            host::serve,
            ipc::Limits,
            runtime::HubRuntime,
            safety::MonotonicClock,
            source::{PollPhase, SourceRegistry},
        };

        let owner = simulated();
        owner.connect().await.unwrap();
        let mut config = HubConfig::empty();
        let source_id = Uuid::new_v4();
        config.sources.push(
            serde_json::from_value(json!({
                "id":source_id,"label":"Retirement fixture",
                "backend":{"kind":"native","device":"camera-sdk","identity":"sim00001",
                    "camera":{"model":"ZWO Simulated"}},
                "polling":{"requestTimeoutSeconds":0.05}
            }))
            .unwrap(),
        );
        let clock = Arc::new(MonotonicClock::default());
        let registry = Arc::new(
            SourceRegistry::build(&config, clock.clone(), |_| {
                Ok(Box::new(NativeCameraBackend::new(owner.clone(), vec![])?))
            })
            .unwrap(),
        );
        let source = registry.get(source_id).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hub.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        let endpoint = Endpoint::for_config(&path).unwrap();
        let runtime = HubRuntime::from_registry(config, registry, clock).unwrap();
        source.acquire(Uuid::new_v4()).await.unwrap();
        owner.retained.wait_idle().await;
        // Keep the actual worker alive behind its serialized engine. Two reset
        // generations must retire along with the final disconnect, even after
        // the source's short request timer has expired.
        let engine = owner.engine.lock().await;
        let unrelated = Activity::new(owner.activity.clone());
        owner.reset();
        owner.reset();
        let stop = CancellationToken::new();
        let stopping = tokio::spawn(serve(
            endpoint.try_lock().unwrap().unwrap().bind().unwrap(),
            runtime.clone(),
            Limits::default(),
            stop.clone(),
        ));
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if source
                    .snapshot()
                    .error
                    .is_some_and(|error| error.kind == ErrorKind::Uncertain)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!stopping.is_finished());
        assert!(endpoint.try_lock().unwrap().is_none());
        assert_ne!(source.snapshot().polling.phase, PollPhase::Stopped);
        assert!(owner.retained.active() >= 3);
        assert!(owner.snapshot().core.process_id.is_some());
        assert_eq!(
            source.acquire(Uuid::new_v4()).await.unwrap_err().kind,
            ErrorKind::Disconnected
        );
        // A dropped shutdown waiter cannot cancel actor-owned retirement.
        stopping.abort();
        let _ = stopping.await;
        drop(engine);
        let result = tokio::time::timeout(Duration::from_secs(5), source.shutdown())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(result.kind, ErrorKind::Uncertain);
        assert_eq!(source.snapshot().polling.phase, PollPhase::Stopped);
        assert_eq!(owner.retained.active(), 0);
        tokio::time::timeout(Duration::from_secs(5), async {
            while endpoint.try_lock().unwrap().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let cleanup = runtime.shutdown().await.unwrap_err();
        assert_eq!(cleanup.len(), 1);
        assert_eq!(cleanup[0].0, source_id);
        assert_eq!(cleanup[0].1.kind, ErrorKind::Uncertain);
        assert_eq!(
            owner.activity.active(),
            1,
            "unrelated work must not block drain"
        );
        assert!(owner.snapshot().core.process_id.is_none());
        assert_eq!(
            source.shutdown().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            owner.retained.active(),
            0,
            "shutdown must not replay cleanup"
        );
        drop(unrelated);
        assert_eq!(owner.activity.active(), 0);
    }

    #[tokio::test]
    async fn queued_imaging_caller_loss_and_expiry_skip_io_without_clearing_the_image() {
        let mut owner = simulated();
        owner.connect().await.unwrap();
        let exposure = NativeGeometry::initial(&owner.snapshot().core.info)
            .unwrap()
            .configured(CameraSetting::NumX(64), &owner.snapshot().core.info)
            .unwrap()
            .configured(CameraSetting::NumY(64), &owner.snapshot().core.info)
            .unwrap()
            .exposure(10_000, true);
        let id = owner.start(exposure).unwrap();
        let reader = owner.wait(id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while owner.activity.active() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        Arc::get_mut(&mut owner).unwrap().command_timeout = Duration::from_millis(50);
        let engine = owner.engine.lock().await;
        let before = owner.snapshot().core.values[&0];
        let mut setter = Box::pin(owner.set_imaging_control(0, 123));
        assert!(futures_util::poll!(setter.as_mut()).is_pending());
        drop(setter);
        tokio::time::timeout(Duration::from_secs(5), async {
            while owner.activity.active() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(owner.snapshot().error.is_none());
        assert_eq!(owner.snapshot().core.values[&0], before);
        tokio::time::pause();
        let mut setter = Box::pin(owner.set_imaging_control(0, 123));
        assert!(futures_util::poll!(setter.as_mut()).is_pending());
        tokio::time::advance(Duration::from_millis(51)).await;
        let error = setter.await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Transient);
        assert!(!error.transport_lost);
        assert!(owner.snapshot().error.is_none());
        assert_eq!(owner.snapshot().core.values[&0], before);
        assert_eq!(
            owner.image().unwrap().bytes().as_ptr(),
            reader.bytes().as_ptr()
        );
        tokio::time::resume();
        drop(engine);
        Arc::get_mut(&mut owner).unwrap().command_timeout = Duration::from_secs(15);
        assert_eq!(owner.set_imaging_control(0, 123).await.unwrap(), 123);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn imaging_write_readback_stays_owned_after_caller_and_last_reference_loss() {
        type Setter =
            std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64, SourceError>> + Send>>;
        let pending: Arc<Mutex<Option<Setter>>> = Arc::new(Mutex::new(None));
        let withdraw = pending.clone();
        let owner = simulated_with_log(Arc::new(move |_, event, _| {
            if event == "control.write_acknowledged" {
                // Drop the actual frontend future after the set ACK and before
                // core issues readback. The owner's task must finish that read.
                withdraw.lock().unwrap().take();
            }
        }));
        owner.connect().await.unwrap();
        let state = owner.status.clone();
        let activity = owner.activity.clone();
        let retained = Arc::downgrade(&owner);
        let caller = owner.clone();
        *pending.lock().unwrap() =
            Some(Box::pin(
                async move { caller.set_imaging_control(0, 123).await },
            ));
        std::future::poll_fn(|cx| {
            let mut pending = pending.lock().unwrap();
            assert!(pending.as_mut().unwrap().as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(owner);
        tokio::time::timeout(Duration::from_secs(5), async {
            while activity.active() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(pending.lock().unwrap().is_none());
        assert_eq!(state.lock().unwrap().values[&0], 123);
        assert!(retained.upgrade().is_none());
    }

    #[tokio::test]
    async fn queued_owner_expiry_never_acquires_the_engine_or_changes_acknowledged_values() {
        let mut owner = simulated();
        Arc::get_mut(&mut owner).unwrap().command_timeout = Duration::from_millis(50);
        owner.connect().await.unwrap();
        let engine = owner.engine.lock().await;
        let before = owner.snapshot().core.values[&16];
        tokio::time::pause();
        let mut setter = Box::pin(owner.set_cooling(16, -15));
        assert!(futures_util::poll!(setter.as_mut()).is_pending());
        assert_eq!(owner.activity.active(), 1);
        // Advance beyond the timer driver's millisecond tick rather than relying
        // on automatic virtual-clock advance while process I/O is live.
        tokio::time::advance(Duration::from_millis(51)).await;
        let error = setter.await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Transient);
        assert!(
            !error.transport_lost,
            "An unsent expiry is not a lost transport"
        );
        assert_eq!(owner.activity.active(), 0);
        assert_eq!(owner.snapshot().core.values[&16], before);
        assert!(owner.snapshot().error.is_none());
        assert!(!owner.cooling.pending());
        tokio::time::resume();
        drop(engine);
        Arc::get_mut(&mut owner).unwrap().command_timeout = Duration::from_secs(15);
        assert_eq!(owner.set_cooling(16, -10).await.unwrap(), -10);
        owner.close().await.unwrap();
    }

    #[tokio::test]
    async fn caller_loss_after_owner_ack_retains_receipt_activity_and_last_owner_reference() {
        let owner = simulated();
        owner.connect().await.unwrap();
        let mut engine = owner.engine.lock().await;
        let mut setter = Box::pin(owner.set_cooling(16, -15));
        assert!(futures_util::poll!(setter.as_mut()).is_pending());
        // Model the capture owner's checkpoint, while its engine lock prevents
        // the idle task from servicing a second command. Its receipt stays owned
        // independently of this external caller, including an unread known ACK.
        engine
            .service_cooling(&CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(owner.snapshot().core.values[&16], -15);
        assert_eq!(owner.activity.active(), 1);
        let activity = owner.activity.clone();
        let retained = Arc::downgrade(&owner);
        drop(setter);
        drop(engine);
        drop(owner);
        assert!(retained.upgrade().is_some());
        tokio::time::timeout(Duration::from_secs(5), async {
            while activity.active() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(retained.upgrade().is_none());
    }

    #[test]
    fn cooler_errors_redact_details_keep_codes_and_distinguish_unsent_expiry() {
        let error = cooling_error(CoolingError::Uncertain {
            message: "private vendor detail".into(),
            code: Some(11),
        });
        assert_eq!(error.kind, ErrorKind::Uncertain);
        assert_eq!(error.upstream_code, Some(11));
        assert!(!error.message.contains("private"));
        let error = cooling_error(CoolingError::Expired);
        assert_eq!(error.kind, ErrorKind::Transient);
        assert!(!error.transport_lost);
        let error = cooling_error(CoolingError::Invalid("private range".into()));
        assert_eq!(error.kind, ErrorKind::InvalidValue);
        assert!(!error.message.contains("private"));
    }
}
