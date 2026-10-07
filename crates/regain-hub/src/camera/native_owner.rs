//! One retained core camera owner per source. Frontend waiters never own its tasks.
//! Client authorization and source leases remain in the outer CameraSupervisor.
use super::{
    image::{CameraImage, ImageBudget},
    native_capture::{NativeCaptureError, capture_admitted},
};
use crate::{
    activity::{Activity, ActivityCounter},
    source::{ErrorKind, SourceError},
};
use regain_core::{
    CancellationToken, Diagnostic, Exposure, Runtime, Selection, Session, SharedStatus, Status,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NativeOperationKind {
    Connecting,
    Capturing,
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
pub struct NativeCameraSnapshot {
    pub generation: Uuid,
    pub connected: bool,
    pub operation: Option<NativeOperation>,
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
struct State {
    generation: Uuid,
    connected: bool,
    pending: Option<Pending>,
    completed: Option<Completed>,
    last_acquisition: Option<Uuid>,
    error: Option<SourceError>,
}
/// Constructed by the native source adapter, sharing the host's budget/activity.
/// This type deliberately exposes no client connection lease or automatic retries.
pub struct NativeCamera {
    engine: AsyncMutex<Session>,
    status: SharedStatus,
    state: Mutex<State>,
    budget: ImageBudget,
    activity: ActivityCounter,
    changed: Notify,
    sdk_fallback: bool,
}

// Unexpected task loss must not leave a permanent Busy marker or claim a usable
// worker. A subsequent explicit connection closes the old session before opening.
struct Work {
    owner: Arc<NativeCamera>,
    generation: Uuid,
    id: Uuid,
    _activity: Activity,
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
            state.pending = None;
            state.connected = false;
            state.error = Some(SourceError::new(
                ErrorKind::Unavailable,
                "Native camera task stopped before completion",
            ));
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

impl NativeCamera {
    /// No discovery or I/O. Simulation is explicitly selected by Runtime.
    pub fn new(
        selection: Selection,
        runtime: Runtime,
        budget: ImageBudget,
        activity: ActivityCounter,
        log: Diagnostic,
    ) -> Result<Arc<Self>, SourceError> {
        let sdk_fallback = selection.direct && selection.sdk_fallback;
        let session = Session::new(selection, runtime, log).map_err(|e| core_error(&e))?;
        Ok(Arc::new(Self {
            status: session.status.clone(),
            engine: AsyncMutex::new(session),
            state: Mutex::new(State {
                generation: Uuid::new_v4(),
                connected: false,
                pending: None,
                completed: None,
                last_acquisition: None,
                error: None,
            }),
            budget,
            activity,
            changed: Notify::new(),
            sdk_fallback,
        }))
    }
    pub fn snapshot(&self) -> NativeCameraSnapshot {
        let state = self.state.lock().unwrap();
        NativeCameraSnapshot {
            generation: state.generation,
            connected: state.connected,
            operation: state.pending.as_ref().map(|p| p.operation.clone()),
            acquisition: state.last_acquisition,
            image_ready: state.connected
                && state.pending.is_none()
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
            _activity: Activity::new(self.activity.clone()),
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
            if state.connected {
                return Ok(());
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
                    let result = session.connect(&token).await.map_err(|e| core_error(&e));
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
                            state.error = result.err();
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
        if !state.connected {
            return Err(disconnected());
        }
        if state.pending.is_some() {
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
                    if !token.is_cancelled() {
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
    pub fn image(&self) -> Result<CameraImage, SourceError> {
        let state = self.state.lock().unwrap();
        if !state.connected {
            return Err(disconnected());
        }
        if state.pending.is_some() {
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
        if let Some(frame) = state
            .completed
            .as_ref()
            .filter(|frame| frame.id == acquisition)
        {
            return Ok(frame.image.clone());
        }
        Err(state.error.clone().unwrap_or_else(|| {
            SourceError::new(
                ErrorKind::Unavailable,
                "Native acquisition has no published image",
            )
        }))
    }
    /// Explicit owner command only. It acknowledges after core cleanup, discards
    /// the active result and never implements StopExposure as AbortExposure.
    pub async fn abort(&self) -> Result<(), SourceError> {
        let (generation, id) = {
            let mut state = self.state.lock().unwrap();
            if !state.connected {
                return Err(disconnected());
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
    fn retire(self: &Arc<Self>) -> Work {
        let generation = Uuid::new_v4();
        let id = Uuid::new_v4();
        let work = self.work(generation, id);
        let mut state = self.state.lock().unwrap();
        if let Some(pending) = state.pending.take() {
            pending.token.cancel();
        }
        state.generation = generation;
        state.connected = false;
        state.completed = None;
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
