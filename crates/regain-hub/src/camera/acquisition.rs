//! One supervisor per source, shared by every output and frontend.
//!
//! Tasks own admission and commands after dispatch. A dropped frontend future
//! never drops an exposure's control lease or sends an implicit AbortExposure.
use super::image::{CameraImage, ImageBudget};
use super::properties::{CameraProperty, CameraSetting, CameraValue};
use crate::{
    activity::{Activity, ActivityCounter},
    readout::SourceLease,
    source::{ErrorKind, SourceError, SourceHandle, Values},
    typed_source::TypedSourceSession,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Notify, oneshot},
    time::{Instant, timeout},
};
use uuid::Uuid;
#[path = "guiding.rs"]
mod guiding;
pub use guiding::{GuideRequest, GuidingPhase, GuidingStatus};

fn invalid(message: &'static str) -> SourceError {
    SourceError::new(ErrorKind::InvalidValue, message)
}
fn unavailable(message: &'static str) -> SourceError {
    SourceError::new(ErrorKind::Unavailable, message)
}
fn busy() -> SourceError {
    SourceError::new(
        ErrorKind::Busy,
        "Another camera operation owns this acquisition",
    )
}

/// Proxy timings. Native sources additionally provide core-derived recovery
/// allowances; proxy cameras do not acquire native retries by republishing.
#[derive(Clone)]
pub struct AcquisitionTiming {
    pub connection_timeout: Duration,
    pub admission_timeout: Duration,
    pub readiness_grace: Duration,
    pub download_timeout: Duration,
    pub poll_interval: Duration,
}
impl Default for AcquisitionTiming {
    fn default() -> Self {
        Self {
            connection_timeout: Duration::from_secs(30),
            admission_timeout: Duration::from_secs(15),
            readiness_grace: Duration::from_secs(30),
            download_timeout: Duration::from_secs(60),
            poll_interval: Duration::from_millis(250),
        }
    }
}
impl AcquisitionTiming {
    fn validate(&self) -> Result<(), SourceError> {
        if self.connection_timeout.is_zero()
            || self.connection_timeout > Duration::from_secs(300)
            || self.admission_timeout.is_zero()
            || self.admission_timeout > Duration::from_secs(3600)
            || self.download_timeout.is_zero()
            || self.download_timeout > Duration::from_secs(3600)
            || self.readiness_grace.is_zero()
            || Instant::now().checked_add(self.readiness_grace).is_none()
            || self.poll_interval < Duration::from_millis(10)
            || self.poll_interval > Duration::from_secs(60)
        {
            return Err(invalid("Invalid camera acquisition timing bounds"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposureRequest {
    pub duration_seconds: f64,
    pub light: bool,
}
impl ExposureRequest {
    fn duration(self) -> Result<Duration, SourceError> {
        Duration::try_from_secs_f64(self.duration_seconds)
            .map_err(|_| invalid("Camera exposure duration must be finite and nonnegative"))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AcquisitionPhase {
    Idle,
    Starting,
    Exposing,
    Reading,
    Downloading,
    Stopping,
    Aborting,
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CaptureGeometry {
    pub width: u32,
    pub height: u32,
    pub bin_x: u32,
    pub bin_y: u32,
    pub start_x: u32,
    pub start_y: u32,
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcquisitionIdentity {
    pub source: Uuid,
    pub generation: Uuid,
    pub acquisition: Uuid,
    pub request: ExposureRequest,
    pub geometry: CaptureGeometry,
    pub exposure: ExposureMetadata,
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposureMetadata {
    pub duration_seconds: Option<f64>,
    pub start_time: Option<String>,
    pub duration_error: Option<SourceError>,
    pub start_time_error: Option<SourceError>,
}
/// The image and this acquisition's metadata are frozen together.
pub struct CapturedImage {
    pub identity: AcquisitionIdentity,
    pub image: CameraImage,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcquisitionStatus {
    pub source: Uuid,
    pub generation: Uuid,
    pub acquisition: Option<Uuid>,
    pub owner: Option<Uuid>,
    pub phase: AcquisitionPhase,
    pub image_ready: bool,
    pub error: Option<SourceError>,
    pub completed: Option<AcquisitionIdentity>,
    pub setting: Option<SettingStatus>,
    pub guiding: Option<GuidingStatus>,
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingStatus {
    pub id: Uuid,
    pub owner: Uuid,
    pub property: CameraProperty,
}
struct Active {
    id: Uuid,
    owner: Uuid,
    generation: Uuid,
    phase: AcquisitionPhase,
    operation: Option<Arc<SourceLease>>,
    command_pending: bool,
    _activity: Activity,
}
#[derive(Default)]
struct State {
    retired: bool,
    active: Option<Active>,
    setting: Option<SettingStatus>,
    guiding: Option<guiding::GuideActive>,
    completed: Option<Arc<CapturedImage>>,
    error: Option<SourceError>,
}
struct SettingWork {
    supervisor: Arc<CameraSupervisor>,
    id: Uuid,
    _activity: Activity,
}
impl Drop for SettingWork {
    fn drop(&mut self) {
        let mut state = self.supervisor.state.lock().unwrap();
        if state
            .setting
            .as_ref()
            .is_some_and(|setting| setting.id == self.id)
        {
            state.setting = None;
            self.supervisor.changed.notify_waiters();
        }
    }
}

pub struct CameraSupervisor {
    source: Arc<SourceHandle>,
    budget: ImageBudget,
    timing: AcquisitionTiming,
    native_timing: Option<regain_core::timing::NativeCameraTiming>,
    activity: ActivityCounter,
    state: Mutex<State>,
    guiding_tasks: Mutex<Vec<guiding::GuideTask>>,
    changed: Notify,
}
impl CameraSupervisor {
    fn readiness_timeout(&self, request: ExposureRequest) -> Result<Duration, SourceError> {
        let duration = request.duration()?;
        let mut ready = duration
            .checked_add(self.timing.readiness_grace)
            .ok_or_else(|| invalid("Camera exposure deadline is not representable"))?;
        if let Some(native) = &self.native_timing {
            let microseconds = u64::try_from(duration.as_micros())
                .map_err(|_| invalid("Native camera exposure duration is not representable"))?;
            ready =
                ready.max(native.capture_allowance(microseconds).map_err(|_| {
                    invalid("Native camera exposure deadline is not representable")
                })?);
        }
        if Instant::now().checked_add(ready).is_none() {
            return Err(invalid("Camera exposure deadline is not representable"));
        }
        Ok(ready)
    }
    fn completion_timeout(&self, ready: Duration) -> Result<Duration, SourceError> {
        // After readiness: two metadata reads before and after copying, the
        // finite download, then explicit control retirement. This outer bound
        // also covers queue waits; expiry retains uncertainty without Abort.
        ready
            .checked_add(self.timing.download_timeout)
            .and_then(|value| value.checked_add(self.source.request_allowance().checked_mul(5)?))
            .and_then(|value| value.checked_add(Duration::from_secs(5)))
            .filter(|value| Instant::now().checked_add(*value).is_some())
            .ok_or_else(|| invalid("Camera completion deadline is not representable"))
    }
    pub(crate) fn capture_timing(
        &self,
        host: Uuid,
        revision: Uuid,
        client: Uuid,
        output: Uuid,
        duration_seconds: f64,
    ) -> Result<super::ipc_timing::CameraCaptureTiming, SourceError> {
        use super::ipc_timing::{CameraCaptureTiming, milliseconds};
        let ready = self.readiness_timeout(ExposureRequest {
            duration_seconds,
            light: true,
        })?;
        Ok(CameraCaptureTiming {
            host_instance: host,
            configuration_revision: revision,
            client_id: client,
            output,
            source: self.source.with_snapshot(|snapshot| snapshot.source),
            native: self.native_timing.is_some(),
            duration_seconds,
            readiness_milliseconds: milliseconds(ready, Duration::ZERO)?,
            completion_milliseconds: milliseconds(self.completion_timeout(ready)?, Duration::ZERO)?,
        })
    }
    /// Inert controller metadata. Derive from the same source write bounds used
    /// in execution; no capture duration or native retry policy is given to proxies.
    pub(crate) fn operation_timing(
        &self,
        host: Uuid,
        revision: Uuid,
        client: Uuid,
        output: Uuid,
        base: Duration,
    ) -> Result<super::ipc_timing::CameraOperationTiming, SourceError> {
        use super::ipc_timing::{CameraOperationTiming, milliseconds};
        let margin = Duration::from_secs(5);
        let read = self.source.request_allowance();
        let admission = self.timing.admission_timeout;
        Ok(CameraOperationTiming {
            host_instance: host,
            configuration_revision: revision,
            client_id: client,
            output,
            source: self.source.with_snapshot(|snapshot| snapshot.source),
            native: self.native_timing.is_some(),
            connect_milliseconds: milliseconds(self.timing.connection_timeout + margin, base)?,
            start_milliseconds: milliseconds(
                admission + self.source.write_allowance("startexposure") + margin,
                base,
            )?,
            setting_milliseconds: milliseconds(
                admission + self.source.write_allowance("gain") + read + margin,
                base,
            )?,
            stop_milliseconds: milliseconds(
                read + self.source.write_allowance("stopexposure") + margin,
                base,
            )?,
            abort_milliseconds: milliseconds(
                read + self.source.write_allowance("abortexposure") + read + margin,
                base,
            )?,
        })
    }
    /// Construction performs no I/O. The runtime must share this instance by
    /// source UUID and supply the same activity counter as its output sessions.
    pub fn new(
        source: Arc<SourceHandle>,
        budget: ImageBudget,
        mut timing: AcquisitionTiming,
        activity: ActivityCounter,
    ) -> Result<Arc<Self>, SourceError> {
        timing.validate()?;
        if source.native_camera_resources().is_some_and(|resources| {
            !resources.shares(&super::runtime::CameraResources::from_parts(
                budget.clone(),
                activity.clone(),
            ))
        }) {
            return Err(invalid(
                "Native camera supervision must share its owner's resources",
            ));
        }
        let native_timing = source.native_camera_timing().cloned();
        if native_timing.is_some() {
            timing.connection_timeout =
                timing.connection_timeout.max(source.connection_allowance());
            if Instant::now()
                .checked_add(timing.connection_timeout)
                .is_none()
            {
                return Err(invalid(
                    "Native camera connection deadline is not representable",
                ));
            }
        }
        Ok(Arc::new(Self {
            source,
            budget,
            timing,
            native_timing,
            activity,
            state: Mutex::new(State::default()),
            guiding_tasks: Mutex::new(Vec::new()),
            changed: Notify::new(),
        }))
    }
    pub async fn connect(self: &Arc<Self>) -> Result<CameraSession, SourceError> {
        if self.state.lock().unwrap().retired {
            return Err(SourceError::new(
                ErrorKind::Disconnected,
                "Camera runtime has retired",
            ));
        }
        let source = timeout(
            self.timing.connection_timeout,
            TypedSourceSession::connect(self.source.clone()),
        )
        .await
        .map_err(|_| SourceError::timeout())??;
        Ok(CameraSession {
            supervisor: self.clone(),
            source: Arc::new(source),
            id: Uuid::new_v4(),
        })
    }
    pub fn status(&self) -> AcquisitionStatus {
        self.status_with_source().1
    }
    pub(crate) fn status_with_source(&self) -> (crate::source::SourceSnapshot, AcquisitionStatus) {
        let source = self.source.snapshot();
        let state = self.state.lock().unwrap();
        let completed = state.completed.as_ref().filter(|image| {
            image.identity.generation == source.generation && source.transport_connected
        });
        let status = AcquisitionStatus {
            source: source.source,
            generation: source.generation,
            acquisition: state.active.as_ref().map(|a| a.id),
            owner: state.active.as_ref().map(|a| a.owner),
            phase: state
                .active
                .as_ref()
                .map_or(AcquisitionPhase::Idle, |a| a.phase),
            image_ready: completed.is_some(),
            error: state.error.clone(),
            completed: completed.map(|image| image.identity.clone()),
            setting: state.setting.clone(),
            guiding: state.guiding.as_ref().map(|guide| {
                let mut status = guide.status.clone();
                if status.generation != source.generation || !source.transport_connected {
                    status.phase = GuidingPhase::Uncertain;
                    status.error.get_or_insert_with(|| {
                        SourceError::new(
                            ErrorKind::Disconnected,
                            "Camera guide source generation is no longer connected",
                        )
                    });
                }
                status
            }),
        };
        (source, status)
    }
    /// The source actor has already closed admission and completed backend drain.
    /// Release local ownership/cache without an Abort, replay or uncertainty reset
    /// on live equipment. Pinned readers keep their own immutable image references.
    pub(crate) async fn retire_after_source_shutdown(&self) {
        let source = self.source.snapshot();
        if source.polling.phase != crate::source::PollPhase::Stopped || source.transport_connected {
            return;
        }
        let tasks = {
            let mut state = self.state.lock().unwrap();
            state.retired = true;
            state.active = None;
            state.guiding = None;
            state.completed = None;
            self.changed.notify_waiters();
            self.guiding_tasks.lock().unwrap().clone()
        };
        for task in tasks {
            let mut handle = task.lock().await;
            if let Some(task) = handle.as_mut() {
                let _ = task.await;
            }
            *handle = None;
        }
        self.guiding_tasks.lock().unwrap().clear();
    }
    /// Explicit administrative abandonment for an orphaned uncertain capture.
    /// A frontend must authorize this setup action; ordinary observers use their
    /// session's owner check. No equipment command or source fence reset is sent.
    pub fn abandon_uncertain(&self, acquisition: Uuid) -> Result<(), SourceError> {
        let mut state = self.state.lock().unwrap();
        let Some(active) = &state.active else {
            return Ok(());
        };
        if active.id != acquisition {
            return Err(unavailable("Camera acquisition changed before abandonment"));
        }
        if active.phase != AcquisitionPhase::Uncertain {
            return Err(busy());
        }
        state.active = None;
        self.changed.notify_waiters();
        Ok(())
    }
    async fn end_rejected(&self, id: Uuid, error: Option<SourceError>) -> Result<(), SourceError> {
        let operation = {
            let mut state = self.state.lock().unwrap();
            let Some(active) = state.active.as_ref().filter(|active| active.id == id) else {
                return Ok(());
            };
            let operation = active.operation.clone();
            if operation
                .as_ref()
                .is_none_or(|operation| Self::guide_retains(&state, operation))
            {
                state.active = None;
                state.error = error;
                self.changed.notify_waiters();
                return Ok(());
            }
            operation.unwrap()
        };
        if let Err(error) = operation.source.control(operation.id, false).await {
            self.uncertain(id, error.clone());
            return Err(error);
        }
        let mut state = self.state.lock().unwrap();
        if state.active.as_ref().is_some_and(|a| a.id == id) {
            state.active = None;
            state.error = error;
        }
        self.changed.notify_waiters();
        Ok(())
    }
    fn uncertain(&self, id: Uuid, error: SourceError) {
        let mut state = self.state.lock().unwrap();
        if let Some(active) = state.active.as_mut().filter(|a| a.id == id) {
            active.phase = AcquisitionPhase::Uncertain;
            active.command_pending = false;
            state.error = Some(error);
            self.changed.notify_waiters();
        }
    }
    fn end_command(&self, id: Uuid, previous: AcquisitionPhase, error: Option<SourceError>) {
        let mut state = self.state.lock().unwrap();
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|a| a.id == id && a.phase != AcquisitionPhase::Uncertain)
        {
            active.command_pending = false;
            active.phase = previous;
            state.error = error;
        }
        self.changed.notify_waiters();
    }
    async fn start(
        self: &Arc<Self>,
        source: Arc<TypedSourceSession>,
        owner: Uuid,
        request: ExposureRequest,
    ) -> Result<Uuid, SourceError> {
        let ready_timeout = self.readiness_timeout(request)?;
        let completion_timeout = self.completion_timeout(ready_timeout)?;
        source.snapshot()?;
        let id = Uuid::new_v4();
        let (reply, response) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            if state.retired {
                return Err(SourceError::new(
                    ErrorKind::Disconnected,
                    "Camera runtime has retired",
                ));
            }
            if state.setting.is_some() {
                return Err(busy());
            }
            let guide_operation = match &state.guiding {
                Some(guide) if guide.status.phase == GuidingPhase::Uncertain => {
                    return Err(SourceError::uncertain());
                }
                Some(guide)
                    if guide.status.owner == owner
                        && guide.status.phase == GuidingPhase::Guiding =>
                {
                    guide.operation.clone()
                }
                Some(_) => return Err(busy()),
                None => None,
            };
            if let Some(active) = &state.active {
                return Err(if active.phase == AcquisitionPhase::Uncertain {
                    SourceError::uncertain()
                } else {
                    busy()
                });
            }
            state.active = Some(Active {
                id,
                owner,
                generation: source.generation(),
                phase: AcquisitionPhase::Starting,
                operation: guide_operation,
                command_pending: false,
                _activity: Activity::new(self.activity.clone()),
            });
        }
        let supervisor = self.clone();
        tokio::spawn(async move {
            supervisor
                .run_start(
                    source,
                    id,
                    request,
                    ready_timeout,
                    completion_timeout,
                    reply,
                )
                .await;
        });
        response
            .await
            .map_err(|_| unavailable("Camera start task stopped"))?
    }
    async fn run_start(
        self: Arc<Self>,
        source: Arc<TypedSourceSession>,
        id: Uuid,
        request: ExposureRequest,
        ready_timeout: Duration,
        completion_timeout: Duration,
        reply: oneshot::Sender<Result<Uuid, SourceError>>,
    ) {
        if reply.is_closed() {
            let _ = self.end_rejected(id, None).await;
            return;
        }
        let prepared = timeout(self.timing.admission_timeout, async {
            let borrowed = self
                .state
                .lock()
                .unwrap()
                .active
                .as_ref()
                .filter(|active| active.id == id)
                .and_then(|active| active.operation.clone());
            let operation = match borrowed {
                Some(operation) => operation,
                None => Arc::new(source.operation().await?),
            };
            {
                let mut state = self.state.lock().unwrap();
                state
                    .active
                    .as_mut()
                    .filter(|active| active.id == id)
                    .ok_or_else(busy)?
                    .operation = Some(operation.clone());
            }
            let geometry = prepare(&source, request).await?;
            Ok::<_, SourceError>((operation, geometry))
        })
        .await
        .unwrap_or_else(|_| Err(SourceError::timeout()));
        let (operation, geometry) = match prepared {
            Ok(value) => value,
            Err(mut error) => {
                if let Err(release) = self.end_rejected(id, Some(error.clone())).await {
                    error = release;
                }
                let _ = reply.send(Err(error));
                return;
            }
        };
        if reply.is_closed() {
            let _ = self.end_rejected(id, None).await;
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            let Some(active) = state.active.as_mut().filter(|a| a.id == id) else {
                return;
            };
            active.operation = Some(operation.clone());
        }
        let result = source
            .write(
                &operation,
                "startexposure",
                Values::from([
                    ("Duration".into(), json!(request.duration_seconds)),
                    ("Light".into(), json!(request.light)),
                ]),
            )
            .await;
        if let Err(mut error) = result {
            if error.kind == ErrorKind::Uncertain || error.transport_lost {
                self.uncertain(id, error.clone());
            } else {
                if let Err(release) = self.end_rejected(id, Some(error.clone())).await {
                    error = release;
                }
            }
            let _ = reply.send(Err(error));
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            let Some(active) = state.active.as_mut().filter(|a| a.id == id) else {
                return;
            };
            active.phase = AcquisitionPhase::Exposing;
            state.completed = None;
            state.error = None;
        }
        let _ = reply.send(Ok(id));
        if timeout(
            completion_timeout,
            self.complete_exposure(&source, id, request, geometry, &operation, ready_timeout),
        )
        .await
        .is_err()
        {
            self.uncertain(
                id,
                SourceError::new(ErrorKind::Uncertain, "Camera completion deadline expired"),
            );
        }
    }
    async fn complete_exposure(
        &self,
        source: &TypedSourceSession,
        id: Uuid,
        request: ExposureRequest,
        geometry: CaptureGeometry,
        operation: &Arc<SourceLease>,
        ready_timeout: Duration,
    ) {
        let result = timeout(ready_timeout, self.wait_ready(source, id))
            .await
            .unwrap_or_else(|_| {
                Err(SourceError::new(
                    ErrorKind::Uncertain,
                    "Camera exposure readiness deadline expired",
                ))
            });
        match result {
            Ok(true) => {}
            Ok(false) => return, // Explicit abort/abandon already reconciled local state.
            Err(error) => {
                self.uncertain(id, error);
                return;
            }
        }
        // Compare available upstream exposure identity around the copy. Optional
        // unsupported metadata is preserved; it cannot prove external ownership.
        let before = match exposure_metadata(source).await {
            Ok(value) => value,
            Err(error) => {
                self.uncertain(id, error);
                return;
            }
        };
        let image = match operation
            .source
            .camera_image_fenced(
                operation.id,
                source.generation(),
                self.budget.clone(),
                self.timing.download_timeout,
            )
            .await
        {
            Ok(image) => image,
            Err(error) => {
                self.uncertain(id, error);
                return;
            }
        };
        let after = match exposure_metadata(source).await {
            Ok(value) => value,
            Err(error) => {
                self.uncertain(id, error);
                return;
            }
        };
        if before.duration_seconds != after.duration_seconds
            || before.start_time != after.start_time
            || image.descriptor().width() != geometry.width
            || image.descriptor().height() != geometry.height
        {
            self.uncertain(
                id,
                unavailable("Camera image differs from the admitted acquisition"),
            );
            return;
        }
        let identity = AcquisitionIdentity {
            source: self.source.snapshot().source,
            generation: source.generation(),
            acquisition: id,
            request,
            geometry,
            exposure: after,
        };
        if let Err(error) = source.snapshot() {
            self.uncertain(id, error);
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.active.as_ref().is_some_and(|active| {
                active.id == id && active.phase == AcquisitionPhase::Downloading
            }) && Self::guide_retains(&state, operation)
            {
                // The pulse still owns this control lease. Publish atomically
                // with removing acquisition ownership so guide completion sees
                // that it must perform the final explicit release.
                state.completed = Some(Arc::new(CapturedImage { identity, image }));
                state.active = None;
                state.error = None;
                self.changed.notify_waiters();
                return;
            }
        }
        if let Err(error) = operation.source.control(operation.id, false).await {
            self.uncertain(id, error);
            return;
        }
        if let Err(error) = source.snapshot() {
            self.uncertain(id, error);
            return;
        }
        let mut state = self.state.lock().unwrap();
        if state
            .active
            .as_ref()
            .is_some_and(|a| a.id == id && a.phase == AcquisitionPhase::Downloading)
        {
            state.completed = Some(Arc::new(CapturedImage { identity, image }));
            state.active = None;
            state.error = None;
        }
    }
    async fn wait_ready(&self, source: &TypedSourceSession, id: Uuid) -> Result<bool, SourceError> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let paused = {
                let state = self.state.lock().unwrap();
                let Some(active) = state.active.as_ref().filter(|a| a.id == id) else {
                    return Ok(false);
                };
                if active.phase == AcquisitionPhase::Uncertain {
                    return Ok(false);
                }
                active.command_pending
                    || state.setting.is_some()
                    || state
                        .guiding
                        .as_ref()
                        .is_some_and(|guide| guide.status.phase == GuidingPhase::Starting)
            };
            if !paused {
                let ready = boolean(source, "imageready").await?;
                let camera_state = integer(source, "camerastate").await?;
                if !(0..=5).contains(&camera_state) {
                    return Err(unavailable("Invalid camera acquisition state"));
                }
                if camera_state == 5 {
                    return Err(unavailable("Upstream camera reports an acquisition error"));
                }
                let mut state = self.state.lock().unwrap();
                let setting_pending = state.setting.is_some()
                    || state
                        .guiding
                        .as_ref()
                        .is_some_and(|guide| guide.status.phase == GuidingPhase::Starting);
                let Some(active) = state.active.as_mut().filter(|a| a.id == id) else {
                    return Ok(false);
                };
                if active.phase == AcquisitionPhase::Uncertain {
                    return Ok(false);
                }
                if !active.command_pending && !setting_pending {
                    if ready {
                        active.phase = AcquisitionPhase::Downloading;
                        return Ok(true);
                    }
                    active.phase = if matches!(camera_state, 3 | 4) {
                        AcquisitionPhase::Reading
                    } else {
                        AcquisitionPhase::Exposing
                    };
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(self.timing.poll_interval) => {},
                _ = changed => {},
            }
        }
    }
    async fn command(
        self: &Arc<Self>,
        source: Arc<TypedSourceSession>,
        owner: Uuid,
        abort: bool,
    ) -> Result<(), SourceError> {
        source.snapshot()?;
        let (reply, response) = oneshot::channel();
        let (admission, idle_cached) = {
            let mut state = self.state.lock().unwrap();
            if state.retired {
                return Err(SourceError::new(
                    ErrorKind::Disconnected,
                    "Camera runtime has retired",
                ));
            }
            if state.active.is_some() && state.setting.is_some() {
                return Err(busy());
            }
            if state
                .guiding
                .as_ref()
                .is_some_and(|guide| guide.status.phase == GuidingPhase::Starting)
            {
                return Err(busy());
            }
            let idle_cached = state.setting.is_some() || state.guiding.is_some();
            let admission = if let Some(active) = state.active.as_mut() {
                if active.owner != owner
                    || active.command_pending
                    || active.phase == AcquisitionPhase::Starting
                {
                    return Err(busy());
                }
                if active.phase == AcquisitionPhase::Uncertain {
                    return Err(SourceError::uncertain());
                }
                if active.generation != source.generation() {
                    return Err(unavailable("Camera acquisition generation changed"));
                }
                if active.phase == AcquisitionPhase::Downloading {
                    return if abort { Err(busy()) } else { Ok(()) };
                } else {
                    let previous = active.phase;
                    active.phase = if abort {
                        AcquisitionPhase::Aborting
                    } else {
                        AcquisitionPhase::Stopping
                    };
                    active.command_pending = true;
                    Some((
                        active.id,
                        active
                            .operation
                            .as_ref()
                            .expect("Admitted camera control")
                            .clone(),
                        previous,
                    ))
                }
            } else {
                None
            };
            (admission, idle_cached)
        };
        let Some((id, operation, previous)) = admission else {
            // Never queue an idle no-op behind an unrelated retained setting or
            // guide. Its observed capability is enough; no actuator is invoked.
            if idle_cached {
                let snapshot = source.snapshot()?;
                let member = command_capability_member(abort);
                if let Some(error) = snapshot.sample_errors.get(member) {
                    return Err(error.clone());
                }
                return validate_command_capability(
                    snapshot
                        .values
                        .get(member)
                        .and_then(|value| value.as_bool())
                        .ok_or_else(|| unavailable("Camera command capability is unavailable"))?,
                );
            }
            return command_capability(&source, abort).await;
        };
        let supervisor = self.clone();
        tokio::spawn(async move {
            if reply.is_closed() {
                supervisor.end_command(id, previous, None);
                return;
            }
            let admitted = command_capability(&source, abort).await;
            if admitted.is_ok() && reply.is_closed() {
                supervisor.end_command(id, previous, None);
                return;
            }
            let admitted = admitted.and_then(|()| {
                let state = supervisor.state.lock().unwrap();
                match state.active.as_ref().filter(|a| a.id == id) {
                    Some(active) if active.phase == AcquisitionPhase::Uncertain => {
                        Err(SourceError::uncertain())
                    }
                    Some(_) => Ok(()),
                    None => Err(unavailable(
                        "Camera acquisition changed before command dispatch",
                    )),
                }
            });
            let result = match admitted {
                Ok(()) => {
                    source
                        .write(
                            &operation,
                            if abort {
                                "abortexposure"
                            } else {
                                "stopexposure"
                            },
                            Values::new(),
                        )
                        .await
                }
                Err(error) => Err(error),
            };
            match &result {
                Err(error) if error.kind == ErrorKind::Uncertain || error.transport_lost => {
                    supervisor.uncertain(id, error.clone())
                }
                Ok(()) if abort => {
                    {
                        let mut state = supervisor.state.lock().unwrap();
                        if state.active.as_ref().is_some_and(|active| active.id == id)
                            && Self::guide_retains(&state, &operation)
                        {
                            state.active = None;
                            state.completed = None;
                            state.error = None;
                            supervisor.changed.notify_waiters();
                            let _ = reply.send(Ok(()));
                            return;
                        }
                    }
                    if let Err(error) = operation.source.control(operation.id, false).await {
                        supervisor.uncertain(id, error.clone());
                        let _ = reply.send(Err(error));
                        return;
                    }
                    let mut state = supervisor.state.lock().unwrap();
                    if state.active.as_ref().is_some_and(|a| a.id == id) {
                        state.active = None;
                        state.completed = None;
                        state.error = None;
                    }
                }
                _ => {
                    supervisor.end_command(id, previous, result.as_ref().err().cloned());
                }
            }
            supervisor.changed.notify_waiters();
            let _ = reply.send(result);
        });
        response
            .await
            .map_err(|_| unavailable("Camera command task stopped"))?
    }
}

async fn command_capability(source: &TypedSourceSession, abort: bool) -> Result<(), SourceError> {
    validate_command_capability(boolean(source, command_capability_member(abort)).await?)
}
fn command_capability_member(abort: bool) -> &'static str {
    if abort {
        "canabortexposure"
    } else {
        "canstopexposure"
    }
}
fn validate_command_capability(supported: bool) -> Result<(), SourceError> {
    if !supported {
        return Err(SourceError::new(
            ErrorKind::Unsupported,
            "Camera does not support this exposure command",
        ));
    }
    Ok(())
}

pub struct CameraSession {
    supervisor: Arc<CameraSupervisor>,
    source: Arc<TypedSourceSession>,
    id: Uuid,
}
impl CameraSession {
    /// Preserve the inner source's per-key observation age/error when composing
    /// outputs. Acquisition identity and published readiness use their own path.
    pub(crate) fn cached_sample(
        &self,
        property: CameraProperty,
        now: Duration,
    ) -> Result<crate::readout::TypedSample<CameraValue>, SourceError> {
        let source = self.source.snapshot()?;
        self.guide_property_error(property)?;
        if let Some(error) = source.error {
            return Err(error);
        }
        let key = property.member();
        if let Some(error) = source.sample_errors.get(key) {
            return Err(error.clone());
        }
        let value = property.decode(
            source
                .values
                .get(key)
                .ok_or_else(|| unavailable("No camera sample has been received"))?,
        )?;
        crate::readout::typed_sample(&source, key, now, value)
    }
    /// Operational state only. No getter refreshes telemetry or copies pixels;
    /// ImageReady describes this supervisor's published acquisition.
    pub(crate) fn device_state(&self, now: Duration) -> Values {
        let Ok(source) = self.source.snapshot() else {
            return Values::new();
        };
        let mut values = Values::new();
        if source.error.is_none() {
            for (property, name) in [
                (CameraProperty::CameraState, "CameraState"),
                (CameraProperty::CcdTemperature, "CCDTemperature"),
                (CameraProperty::CoolerPower, "CoolerPower"),
                (CameraProperty::HeatSinkTemperature, "HeatSinkTemperature"),
                (CameraProperty::IsPulseGuiding, "IsPulseGuiding"),
                (CameraProperty::PercentCompleted, "PercentCompleted"),
            ] {
                let key = property.member();
                if source.sample_errors.contains_key(key)
                    || self.guide_property_error(property).is_err()
                {
                    continue;
                }
                if let Some(value) = source.values.get(key)
                    && let Ok(value) = property.decode(value)
                    && crate::readout::typed_sample(&source, key, now, ()).is_ok()
                {
                    values.insert(name.into(), value.into_value());
                }
            }
        }
        let status = self.supervisor.status();
        if status.error.is_none() {
            values.insert("ImageReady".into(), json!(status.image_ready));
        }
        values
    }
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn connected(&self) -> bool {
        self.source.connected()
    }
    pub fn status(&self) -> AcquisitionStatus {
        self.supervisor.status()
    }
    /// Standard image readiness/timing belongs to our published acquisition,
    /// never to a later unowned exposure in an upstream driver's buffer.
    pub async fn property(&self, property: CameraProperty) -> Result<CameraValue, SourceError> {
        self.source.snapshot()?;
        self.guide_property_error(property)?;
        let value = match property {
            CameraProperty::ImageReady => {
                let status = self.supervisor.status();
                if !status.image_ready
                    && let Some(error) = status.error
                {
                    return Err(error);
                }
                CameraValue::Boolean {
                    value: status.image_ready,
                }
            }
            CameraProperty::LastExposureDuration => {
                let image = self.image()?;
                match image.identity.exposure.duration_seconds {
                    Some(value) => CameraValue::Number { value },
                    None => {
                        return Err(image
                            .identity
                            .exposure
                            .duration_error
                            .clone()
                            .unwrap_or_else(|| {
                                unavailable("Camera exposure duration unavailable")
                            }));
                    }
                }
            }
            CameraProperty::LastExposureStartTime => {
                let image = self.image()?;
                match &image.identity.exposure.start_time {
                    Some(value) => CameraValue::Text {
                        value: value.clone(),
                    },
                    None => {
                        return Err(image
                            .identity
                            .exposure
                            .start_time_error
                            .clone()
                            .unwrap_or_else(|| {
                                unavailable("Camera exposure start time unavailable")
                            }));
                    }
                }
            }
            _ => property.read(&self.source).await?,
        };
        self.source.snapshot()?;
        Ok(value)
    }
    /// A dropped setter waiter does not release a dispatched write's ownership.
    /// No setting is replayed, compensated, or locally cached as a hardware fact.
    pub async fn set(&self, setting: CameraSetting) -> Result<(), SourceError> {
        setting.validate()?;
        if self.supervisor.source.snapshot().write_uncertain {
            return Err(SourceError::uncertain());
        }
        self.source.snapshot()?;
        let source = self.source.clone();
        let supervisor = self.supervisor.clone();
        let (work, acquisition_operation, acquisition) = {
            let mut state = supervisor.state.lock().unwrap();
            if state.retired {
                return Err(SourceError::new(
                    ErrorKind::Disconnected,
                    "Camera runtime has retired",
                ));
            }
            let operation = settings_operation(&state, setting, self.id)?;
            let id = Uuid::new_v4();
            state.setting = Some(SettingStatus {
                id,
                owner: self.id,
                property: setting.property(),
            });
            (
                SettingWork {
                    supervisor: supervisor.clone(),
                    id,
                    _activity: Activity::new(supervisor.activity.clone()),
                },
                operation,
                state.active.as_ref().map(|active| active.id),
            )
        };
        let (reply, response) = oneshot::channel();
        tokio::spawn(async move {
            let release_control = acquisition_operation.is_none();
            let result = async {
                if reply.is_closed() {
                    return Ok(());
                }
                let operation = timeout(supervisor.timing.admission_timeout, async {
                    let operation = match acquisition_operation {
                        Some(operation) => operation,
                        None => Arc::new(source.operation().await?),
                    };
                    setting.preflight(&source).await?;
                    Ok::<_, SourceError>(operation)
                })
                .await
                .map_err(|_| SourceError::timeout())??;
                if reply.is_closed() {
                    return Ok(());
                }
                if let Some(id) = acquisition {
                    let state = supervisor.state.lock().unwrap();
                    let active = state
                        .active
                        .as_ref()
                        .filter(|active| active.id == id)
                        .ok_or_else(|| {
                            unavailable("Camera acquisition changed before setting dispatch")
                        })?;
                    if active.phase == AcquisitionPhase::Uncertain {
                        return Err(SourceError::uncertain());
                    }
                    if active.command_pending
                        || !matches!(
                            active.phase,
                            AcquisitionPhase::Exposing | AcquisitionPhase::Reading
                        )
                    {
                        return Err(busy());
                    }
                }
                let result = source
                    .write(
                        &operation,
                        setting.property().member(),
                        setting.parameters(),
                    )
                    .await;
                // Explicit local release completes before acknowledging this
                // setter, so an immediate next setter does not race Drop cleanup.
                let release = if release_control {
                    operation.source.control(operation.id, false).await
                } else {
                    Ok(())
                };
                result?;
                release
            }
            .await;
            drop(work);
            let _ = reply.send(result);
        });
        response
            .await
            .map_err(|_| unavailable("Camera setting task stopped"))?
    }
    pub async fn start(&self, request: ExposureRequest) -> Result<Uuid, SourceError> {
        self.supervisor
            .start(self.source.clone(), self.id, request)
            .await
    }
    pub async fn abort(&self) -> Result<(), SourceError> {
        self.supervisor
            .command(self.source.clone(), self.id, true)
            .await
    }
    pub async fn stop(&self) -> Result<(), SourceError> {
        self.supervisor
            .command(self.source.clone(), self.id, false)
            .await
    }
    pub fn image(&self) -> Result<Arc<CapturedImage>, SourceError> {
        let source = self.source.snapshot()?;
        let image = self
            .supervisor
            .state
            .lock()
            .unwrap()
            .completed
            .as_ref()
            .filter(|image| image.identity.generation == source.generation)
            .cloned()
            .ok_or_else(|| {
                unavailable("No completed image is available for this camera generation")
            })?;
        self.source.snapshot()?;
        Ok(image)
    }
    /// Explicitly abandon only local uncertain ownership. No equipment command
    /// or source fence reset is sent; clients must reconnect a changed generation.
    pub fn abandon_uncertain(&self) -> Result<(), SourceError> {
        let state = self.supervisor.state.lock().unwrap();
        let Some(active) = &state.active else {
            return Ok(());
        };
        if active.owner != self.id {
            return Err(busy());
        }
        if active.phase != AcquisitionPhase::Uncertain {
            return Err(busy());
        }
        let id = active.id;
        drop(state);
        self.supervisor.abandon_uncertain(id)
    }
}
fn settings_operation(
    state: &State,
    setting: CameraSetting,
    owner: Uuid,
) -> Result<Option<Arc<SourceLease>>, SourceError> {
    if let Some(guide) = &state.guiding {
        return Err(if guide.status.phase == GuidingPhase::Uncertain {
            SourceError::uncertain()
        } else {
            busy()
        });
    }
    if state.setting.is_some() {
        return Err(busy());
    }
    match state.active.as_ref() {
        None => Ok(None),
        Some(active) if active.phase == AcquisitionPhase::Uncertain => {
            Err(SourceError::uncertain())
        }
        Some(active)
            if active.owner == owner
                && !setting.changes_capture()
                && !active.command_pending
                && matches!(
                    active.phase,
                    AcquisitionPhase::Exposing | AcquisitionPhase::Reading
                ) =>
        {
            active.operation.clone().map(Some).ok_or_else(busy)
        }
        Some(_) => Err(busy()),
    }
}

async fn integer(source: &TypedSourceSession, member: &str) -> Result<i32, SourceError> {
    source
        .read(member)
        .await?
        .as_i64()
        .and_then(|v| i32::try_from(v).ok())
        .ok_or_else(|| unavailable("Camera property is not an Int32"))
}
async fn boolean(source: &TypedSourceSession, member: &str) -> Result<bool, SourceError> {
    source
        .read(member)
        .await?
        .as_bool()
        .ok_or_else(|| unavailable("Camera property is not Boolean"))
}
async fn number(source: &TypedSourceSession, member: &str) -> Result<f64, SourceError> {
    source
        .read(member)
        .await?
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| unavailable("Camera property is not a finite number"))
}
async fn prepare(
    source: &TypedSourceSession,
    request: ExposureRequest,
) -> Result<CaptureGeometry, SourceError> {
    let camera_state = integer(source, "camerastate").await?;
    if !(0..=5).contains(&camera_state) {
        return Err(unavailable("Invalid camera state before exposure"));
    }
    if camera_state != 0 {
        return Err(busy());
    }
    let minimum = number(source, "exposuremin").await?;
    let maximum = number(source, "exposuremax").await?;
    if minimum < 0.0 || maximum < minimum || maximum <= 0.0 {
        return Err(unavailable("Invalid camera exposure limits"));
    }
    if request.duration_seconds > maximum || (request.light && request.duration_seconds < minimum) {
        return Err(invalid(
            "Camera exposure duration is outside the source limits",
        ));
    }
    let mut values = Vec::new();
    for member in [
        "numx",
        "numy",
        "binx",
        "biny",
        "startx",
        "starty",
        "cameraxsize",
        "cameraysize",
        "maxbinx",
        "maxbiny",
    ] {
        let value = integer(source, member).await?;
        if value < 0 || (value == 0 && !matches!(member, "startx" | "starty")) {
            return Err(invalid("Camera exposure geometry contains invalid values"));
        }
        values.push(value as u32);
    }
    let g = CaptureGeometry {
        width: values[0],
        height: values[1],
        bin_x: values[2],
        bin_y: values[3],
        start_x: values[4],
        start_y: values[5],
    };
    if g.bin_x > values[8]
        || g.bin_y > values[9]
        || ((u64::from(g.start_x) + u64::from(g.width)) * u64::from(g.bin_x)) > u64::from(values[6])
        || ((u64::from(g.start_y) + u64::from(g.height)) * u64::from(g.bin_y))
            > u64::from(values[7])
        || (!boolean(source, "canasymmetricbin").await? && g.bin_x != g.bin_y)
    {
        return Err(invalid(
            "Camera binning/subframe combination is outside the source limits",
        ));
    }
    Ok(g)
}
async fn exposure_metadata(source: &TypedSourceSession) -> Result<ExposureMetadata, SourceError> {
    let (duration_seconds, duration_error) = match number(source, "lastexposureduration").await {
        Ok(value) if value >= 0.0 => (Some(value), None),
        Ok(_) => return Err(unavailable("Invalid completed camera exposure duration")),
        Err(error) if error.kind == ErrorKind::Unsupported => (None, Some(error)),
        Err(error) => return Err(error),
    };
    let (start_time, start_time_error) = match source.read("lastexposurestarttime").await {
        Ok(value) => {
            let value = value
                .as_str()
                .filter(|v| valid_start_time(v))
                .ok_or_else(|| unavailable("Invalid camera exposure start time"))?;
            (Some(value.to_owned()), None)
        }
        Err(error) if error.kind == ErrorKind::Unsupported => (None, Some(error)),
        Err(error) => return Err(error),
    };
    Ok(ExposureMetadata {
        duration_seconds,
        start_time,
        duration_error,
        start_time_error,
    })
}
pub(super) fn valid_start_time(value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    let value = value
        .strip_suffix('Z')
        .or_else(|| value.strip_suffix("+00:00"))
        .unwrap_or(value);
    let bytes = value.as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return false;
    }
    if !bytes[..19]
        .iter()
        .enumerate()
        .all(|(i, v)| matches!(i, 4 | 7 | 10 | 13 | 16) || v.is_ascii_digit())
    {
        return false;
    }
    let number = |start: usize, length: usize| {
        bytes[start..start + length]
            .iter()
            .fold(0u32, |n, digit| n * 10 + u32::from(digit - b'0'))
    };
    let year = number(0, 4);
    let month = number(5, 2);
    let day = number(8, 2);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => return false,
    };
    if day == 0 || day > days || number(11, 2) > 23 || number(14, 2) > 59 || number(17, 2) > 60 {
        return false;
    }
    bytes.len() == 19
        || (bytes.len() > 20 && bytes[19] == b'.' && bytes[20..].iter().all(u8::is_ascii_digit))
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    #[tokio::test]
    async fn native_supervisor_honors_a_saved_connection_allowance_above_the_core_ceiling() {
        use crate::{
            camera::{native_owner::NativeCamera, native_source::NativeCameraBackend},
            parameters::PollPolicy,
            safety::MonotonicClock,
        };
        let budget = ImageBudget::new(1024).unwrap();
        let activity = ActivityCounter::default();
        let selection = regain_core::Selection {
            name: "ZWO Simulated".into(),
            serial: None,
            direct: false,
            sdk_fallback: false,
            recovery: regain_core::RecoveryOptions {
                command_timeout_seconds: 0.001,
                ..regain_core::RecoveryOptions::default()
            },
        };
        let native = regain_core::timing::NativeCameraTiming::new(&selection).unwrap();
        assert!(native.connection_allowance() < Duration::from_secs(300));
        let owner = NativeCamera::new(
            selection,
            regain_core::Runtime {
                directory: "absent-fixture-workers".into(),
                sdk: "absent-fixture-sdk".into(),
                simulate: true,
                sdk_simulation: None,
            },
            budget.clone(),
            activity.clone(),
            Arc::new(|_, _, _| {}),
        )
        .unwrap();
        let source = SourceHandle::spawn(
            Uuid::new_v4(),
            Uuid::new_v4(),
            PollPolicy {
                connection_timeout_seconds: 300.,
                ..PollPolicy::default()
            },
            Box::new(NativeCameraBackend::new(owner, vec![]).unwrap()),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let supervisor = CameraSupervisor::new(
            source.clone(),
            budget.clone(),
            AcquisitionTiming::default(),
            activity.clone(),
        )
        .unwrap();
        assert_eq!(
            supervisor.timing.connection_timeout,
            Duration::from_secs(300)
        );
        assert_eq!(
            source.connection_allowance(),
            supervisor.timing.connection_timeout
        );
        assert_eq!(source.snapshot().lease_count, 0);
        assert_eq!(activity.active(), 0);
        assert_eq!(budget.used_bytes(), 0);
        drop(supervisor);
        source.shutdown().await.unwrap();
    }
}
