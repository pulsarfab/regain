//! Explicit camera bursts through the ordinary acquisition supervisor.
use crate::{
    camera::acquisition::{
        AcquisitionIdentity, CameraCaptureProfile, CameraDispatch, CameraSession,
        CameraStartReceipt, CapturedImage, ExposureRequest, PreparedCameraStart,
    },
    readout::invalid,
    source::{ErrorKind, SourceError},
};
use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use regain_core::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Mutex as AsyncMutex, watch},
    time::{Instant, sleep_until},
};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CameraFailurePolicy {
    Continue,
    AbortStarted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CameraCancellationPolicy {
    LeaveRunning,
    AbortStarted,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraGroupConfig {
    #[schemars(extend("readOnly" = true, "x-regain" = {"immutableAfterCreate":true}))]
    pub id: Uuid,
    #[schemars(length(min = 1, max = 200))]
    pub label: String,
    /// Whole-operation bound. Admitted mutations retain their actor deadline.
    #[schemars(range(min = 0.01, max = 604800.0), extend("default" = 300.0, "x-regain" = {"units":"s"}))]
    pub timeout_seconds: f64,
    /// Continue healthy captures, or explicitly abort acknowledged siblings.
    pub failure_policy: CameraFailurePolicy,
    /// Cancellation and the group deadline use this explicit policy.
    pub cancellation_policy: CameraCancellationPolicy,
    /// Resolved physical source IDs, not enumeration indices.
    #[schemars(length(min = 2, max = 32), extend("items" = {"type":"string","format":"uuid","x-regain":{"reference":"source","deviceType":"camera"}}))]
    pub members: Vec<Uuid>,
}
impl CameraGroupConfig {
    pub fn validate(&self) -> Result<(), SourceError> {
        let unique: BTreeSet<_> = self.members.iter().copied().collect();
        if self.id.is_nil()
            || self.label.trim().is_empty()
            || self.label.chars().count() > 200
            || !self.timeout_seconds.is_finite()
            || !(0.01..=604800.0).contains(&self.timeout_seconds)
            || !(2..=32).contains(&self.members.len())
            || unique.len() != self.members.len()
            || unique.contains(&Uuid::nil())
        {
            return Err(invalid("Invalid camera group configuration"));
        }
        Ok(())
    }
    fn requires_abort(&self) -> bool {
        self.failure_policy == CameraFailurePolicy::AbortStarted
            || self.cancellation_policy == CameraCancellationPolicy::AbortStarted
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraMemberRequest {
    pub source: Uuid,
    pub exposure: ExposureRequest,
    /// Consumers such as NINA require frozen, supported scalar sensor metadata.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub require_scalar_image: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CameraMemberPhase {
    NotStarted,
    Prepared,
    Starting,
    Exposing,
    Aborting,
    Complete,
    Rejected,
    Failed,
    Uncertain,
    Aborted,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CameraGroupPhase {
    Preflight,
    Capturing,
    Complete,
    PreflightFailed,
    PartialFailure,
    Cancelled,
    Deadline,
}
impl CameraGroupPhase {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Preflight | Self::Capturing)
    }
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraMemberResult {
    pub source: Uuid,
    pub generation: Uuid,
    pub request: ExposureRequest,
    pub require_scalar_image: bool,
    pub capture_profile: Option<CameraCaptureProfile>,
    pub acquisition: Option<Uuid>,
    pub phase: CameraMemberPhase,
    /// Monotonic host request/ack windows relative to operation admission.
    pub dispatch_seconds: Option<f64>,
    pub acknowledgement_seconds: Option<f64>,
    pub image: Option<AcquisitionIdentity>,
    pub error: Option<SourceError>,
    pub abort_error: Option<SourceError>,
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraGroupResult {
    pub operation: Uuid,
    pub group: Uuid,
    pub sequence: u64,
    pub phase: CameraGroupPhase,
    /// Spread of observed host dispatch times, never a sensor synchronization claim.
    /// None until every member's start window is known.
    pub start_skew_seconds: Option<f64>,
    pub members: Vec<CameraMemberResult>,
}
type Images = Arc<Mutex<Vec<Option<Arc<CapturedImage>>>>>;
pub struct CameraGroup {
    config: CameraGroupConfig,
    sessions: Vec<Arc<CameraSession>>,
    active: Arc<AsyncMutex<()>>,
}
impl CameraGroup {
    pub fn new(
        config: CameraGroupConfig,
        sessions: Vec<Arc<CameraSession>>,
    ) -> Result<Arc<Self>, SourceError> {
        config.validate()?;
        if sessions.len() != config.members.len()
            || sessions
                .iter()
                .zip(&config.members)
                .any(|(s, id)| s.source_id() != *id)
        {
            return Err(invalid(
                "Camera group sessions do not match physical sources",
            ));
        }
        Ok(Arc::new(Self {
            config,
            sessions,
            active: Arc::new(AsyncMutex::new(())),
        }))
    }
    /// A dropped observer does not cancel equipment. Requests match stable source
    /// IDs exactly; callers cannot silently retarget by changing enumeration order.
    pub fn start(
        self: &Arc<Self>,
        requests: Vec<CameraMemberRequest>,
    ) -> Result<CameraGroupOperation, SourceError> {
        let origin = Instant::now();
        self.start_owned(
            requests,
            Uuid::new_v4(),
            CancellationToken::new(),
            origin,
            origin + Duration::from_secs_f64(self.config.timeout_seconds),
            None,
        )
    }
    pub(crate) fn start_owned(
        self: &Arc<Self>,
        requests: Vec<CameraMemberRequest>,
        operation: Uuid,
        cancel: CancellationToken,
        origin: Instant,
        deadline: Instant,
        retained: Option<crate::activity::Activity>,
    ) -> Result<CameraGroupOperation, SourceError> {
        if requests.len() != self.sessions.len()
            || requests
                .iter()
                .zip(&self.config.members)
                .any(|(r, id)| r.source != *id)
        {
            return Err(invalid("Camera requests do not match configured members"));
        }
        // Reject malformed durations before reserving any source.
        for request in &requests {
            if !request.exposure.duration_seconds.is_finite()
                || request.exposure.duration_seconds < 0.0
            {
                return Err(invalid("Invalid camera exposure duration"));
            }
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| invalid("Camera group requires an active runtime"))?;
        let active = self.active.clone().try_lock_owned().map_err(|_| {
            SourceError::new(ErrorKind::Busy, "Camera group operation is already active")
        })?;
        let report = CameraGroupResult {
            operation,
            group: self.config.id,
            sequence: 1,
            phase: CameraGroupPhase::Preflight,
            start_skew_seconds: None,
            members: self
                .sessions
                .iter()
                .zip(requests)
                .map(|(s, r)| CameraMemberResult {
                    source: r.source,
                    generation: s.generation(),
                    request: r.exposure,
                    require_scalar_image: r.require_scalar_image,
                    capture_profile: None,
                    acquisition: None,
                    phase: CameraMemberPhase::NotStarted,
                    dispatch_seconds: None,
                    acknowledgement_seconds: None,
                    image: None,
                    error: None,
                    abort_error: None,
                })
                .collect(),
        };
        let (publish, status) = watch::channel(report);
        let (finish, finished) = watch::channel(false);
        let task_cancel = cancel.clone();
        let images: Images = Arc::new(Mutex::new(vec![None; self.sessions.len()]));
        let task_images = images.clone();
        let group = self.clone();
        let guard = GroupTaskGuard {
            publish: publish.clone(),
            finish,
            finished: false,
        };
        runtime.spawn(async move {
            group
                .run(&publish, task_cancel, origin, deadline, task_images)
                .await;
            drop(active);
            drop(retained);
            guard.finish();
        });
        Ok(CameraGroupOperation {
            status,
            cancel,
            finished,
            images,
        })
    }
    async fn run(
        &self,
        publish: &watch::Sender<CameraGroupResult>,
        cancel: CancellationToken,
        origin: Instant,
        deadline: Instant,
        images: Images,
    ) {
        let mut report = publish.borrow().clone();
        let mut prepared: Vec<PreparedCameraStart> = Vec::with_capacity(self.sessions.len());
        // The ordinary supervisor owns preparation cleanup even if interruption
        // drops this waiter. No StartExposure can occur during this stage.
        for (index, session) in self.sessions.iter().enumerate() {
            let result = tokio::select! { biased;
                _ = cancel.cancelled() => Err(CameraGroupPhase::Cancelled),
                _ = sleep_until(deadline) => Err(CameraGroupPhase::Deadline),
                result = session.prepare_group(report.members[index].request, report.members[index].require_scalar_image) => Ok(result),
            };
            match result {
                Ok(Ok(value)) => {
                    report.members[index].capture_profile = value.capture_profile();
                    prepared.push(value);
                    report.members[index].phase = CameraMemberPhase::Prepared;
                    emit(publish, &mut report);
                }
                result => {
                    report.phase = match result {
                        Ok(Err(error)) => {
                            reject(&mut report.members[index], error);
                            CameraGroupPhase::PreflightFailed
                        }
                        Err(phase) => phase,
                        _ => unreachable!(),
                    };
                    release(prepared, &mut report).await;
                    emit(publish, &mut report);
                    return;
                }
            }
        }
        // Recheck every live geometry and required abort capability before the
        // burst. Settings remain reserved; no implicit ROI/configuration writes.
        for (index, reservation) in prepared.iter().enumerate() {
            let result = tokio::select! { biased;
                _ = cancel.cancelled() => Err(CameraGroupPhase::Cancelled),
                _ = sleep_until(deadline) => Err(CameraGroupPhase::Deadline),
                result = reservation.recheck(self.config.requires_abort()) => Ok(result),
            };
            match result {
                Ok(Ok(())) => {}
                result => {
                    report.phase = match result {
                        Ok(Err(error)) => {
                            reject(&mut report.members[index], error);
                            CameraGroupPhase::PreflightFailed
                        }
                        Err(phase) => phase,
                        _ => unreachable!(),
                    };
                    release(prepared, &mut report).await;
                    emit(publish, &mut report);
                    return;
                }
            }
        }
        if let Some(phase) = interruption(&cancel, deadline) {
            report.phase = phase;
            release(prepared, &mut report).await;
            emit(publish, &mut report);
            return;
        }
        let mut events: FuturesUnordered<BoxFuture<'static, Event>> = FuturesUnordered::new();
        // No await between dispatches: separate source actors get one request each.
        for (index, reservation) in prepared.into_iter().enumerate() {
            let CameraDispatch {
                acquisition,
                started,
                completed,
            } = reservation.dispatch();
            report.members[index].acquisition = Some(acquisition);
            report.members[index].phase = CameraMemberPhase::Starting;
            events.push(
                async move {
                    Event::Start(
                        index,
                        started.await.map_err(|_| SourceError::uncertain()),
                        completed,
                    )
                }
                .boxed(),
            );
        }
        report.phase = CameraGroupPhase::Capturing;
        emit(publish, &mut report);
        let mut starts_pending = self.sessions.len();
        let mut aborts_pending = 0;
        let mut abort_requested = vec![false; self.sessions.len()];
        let mut pending_images = vec![false; self.sessions.len()];
        let mut stop = None;
        let mut deadline_hit = false;
        let mut failure = false;
        loop {
            let abort = (failure
                && self.config.failure_policy == CameraFailurePolicy::AbortStarted)
                || (stop.is_some()
                    && self.config.cancellation_policy == CameraCancellationPolicy::AbortStarted);
            if abort {
                for (index, member) in report.members.iter_mut().enumerate() {
                    if member.phase == CameraMemberPhase::Exposing && !abort_requested[index] {
                        abort_requested[index] = true;
                        member.phase = CameraMemberPhase::Aborting;
                        aborts_pending += 1;
                        let session = self.sessions[index].clone();
                        let acquisition = member.acquisition.expect("Dispatched acquisition");
                        events.push(async move {Event::Abort(index,session.abort_group(acquisition).await)}.boxed());
                    }
                }
                emit(publish, &mut report);
            }
            let finishing = starts_pending == 0
                && aborts_pending == 0
                && (stop.is_some() || abort)
                // A rejected abort during readout must still retain the admitted
                // image. Its supervisor owns a bounded completion; the group
                // additionally stops waiting at its own deadline.
                && (!abort || !pending_images.iter().any(|pending| *pending) || deadline_hit);
            // Consume already-ready image pins before freezing a stopped report.
            let event = if finishing {
                match events.next().now_or_never().flatten() {
                    Some(event) => event,
                    None => break,
                }
            } else {
                tokio::select! { biased;
                    event = events.next() => { match event { Some(event) => event, None => break } },
                    _ = cancel.cancelled(), if stop.is_none() => {stop = Some(CameraGroupPhase::Cancelled); continue;},
                    _ = sleep_until(deadline), if !deadline_hit => {deadline_hit = true; if stop.is_none() {stop = Some(CameraGroupPhase::Deadline);} continue;},
                }
            };
            match event {
                Event::Start(index, receipt, completed) => {
                    starts_pending -= 1;
                    let member = &mut report.members[index];
                    let result = receipt.and_then(|receipt| {
                        member.dispatch_seconds =
                            Some(receipt.dispatched_at.duration_since(origin).as_secs_f64());
                        member.acknowledgement_seconds =
                            Some(receipt.acknowledged_at.duration_since(origin).as_secs_f64());
                        receipt.result
                    });
                    match result {
                        Ok(id) if Some(id) == member.acquisition => {
                            pending_images[index] = true;
                            member.phase = CameraMemberPhase::Exposing;
                            events.push(
                                async move {
                                    Event::Image(
                                        index,
                                        completed
                                            .await
                                            .unwrap_or_else(|_| Err(SourceError::uncertain())),
                                    )
                                }
                                .boxed(),
                            );
                        }
                        result => {
                            fail(member, result.err().unwrap_or_else(SourceError::uncertain));
                            failure = true;
                        }
                    }
                    if starts_pending == 0
                        && report.members.iter().all(|m| m.dispatch_seconds.is_some())
                    {
                        let times = report.members.iter().filter_map(|m| m.dispatch_seconds);
                        report.start_skew_seconds = Some(
                            times.clone().fold(f64::NEG_INFINITY, f64::max)
                                - times.fold(f64::INFINITY, f64::min),
                        );
                    }
                }
                Event::Image(index, result) => {
                    pending_images[index] = false;
                    let member = &mut report.members[index];
                    match result {
                        Ok(image)
                            if image.identity.source == member.source
                                && image.identity.generation == member.generation
                                && Some(image.identity.acquisition) == member.acquisition =>
                        {
                            member.image = Some(image.identity.clone());
                            images.lock().unwrap()[index] = Some(image);
                            member.phase = CameraMemberPhase::Complete;
                            member.error = None;
                        }
                        result => {
                            if !matches!(
                                member.phase,
                                CameraMemberPhase::Aborting | CameraMemberPhase::Aborted
                            ) {
                                // Completion failures fence the ordinary supervisor
                                // even when their cause is a deterministic budget or
                                // shape error. Preserve that ownership uncertainty.
                                member.phase = CameraMemberPhase::Uncertain;
                                member.error =
                                    Some(result.err().unwrap_or_else(SourceError::uncertain));
                                failure = true;
                            }
                        }
                    }
                }
                Event::Abort(index, result) => {
                    aborts_pending -= 1;
                    let member = &mut report.members[index];
                    match result {
                        Ok(()) if member.phase != CameraMemberPhase::Complete => {
                            pending_images[index] = false;
                            member.phase = CameraMemberPhase::Aborted
                        }
                        Ok(()) => {}
                        Err(error) => {
                            member.abort_error = Some(error);
                            if member.phase != CameraMemberPhase::Complete {
                                member.phase = CameraMemberPhase::Uncertain;
                            }
                        }
                    }
                }
            }
            emit(publish, &mut report);
        }
        report.phase = stop.unwrap_or_else(|| {
            if report
                .members
                .iter()
                .all(|m| m.phase == CameraMemberPhase::Complete)
            {
                CameraGroupPhase::Complete
            } else {
                CameraGroupPhase::PartialFailure
            }
        });
        // Cancellation/deadline cannot disappear just because the last actor
        // acknowledgement was delivered in the same scheduling turn.
        if let Some(phase) = interruption(&cancel, deadline) {
            report.phase = phase;
        }
        emit(publish, &mut report);
    }
}
/// A stopped task cannot leave observers believing the group is still running.
/// Camera supervisors independently retain their own control/activity guards.
struct GroupTaskGuard {
    publish: watch::Sender<CameraGroupResult>,
    finish: watch::Sender<bool>,
    finished: bool,
}
impl GroupTaskGuard {
    fn finish(mut self) {
        self.finished = true;
        self.finish.send_replace(true);
    }
}
impl Drop for GroupTaskGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut report = self.publish.borrow().clone();
        if !report.phase.terminal() {
            for member in &mut report.members {
                if !matches!(
                    member.phase,
                    CameraMemberPhase::Complete
                        | CameraMemberPhase::Aborted
                        | CameraMemberPhase::Rejected
                        | CameraMemberPhase::Failed
                        | CameraMemberPhase::Uncertain
                ) {
                    member.phase = CameraMemberPhase::Uncertain;
                    member.error = Some(SourceError::uncertain());
                }
            }
            report.phase = CameraGroupPhase::PartialFailure;
            emit(&self.publish, &mut report);
        }
    }
}
enum Event {
    Start(
        usize,
        Result<CameraStartReceipt, SourceError>,
        tokio::sync::oneshot::Receiver<Result<Arc<CapturedImage>, SourceError>>,
    ),
    Image(usize, Result<Arc<CapturedImage>, SourceError>),
    Abort(usize, Result<(), SourceError>),
}
async fn release(prepared: Vec<PreparedCameraStart>, report: &mut CameraGroupResult) {
    for (index, reservation) in prepared.into_iter().enumerate() {
        if let Err(error) = reservation.release(None).await {
            fail(&mut report.members[index], error);
        } else if report.members[index].phase == CameraMemberPhase::Prepared {
            report.members[index].phase = CameraMemberPhase::NotStarted;
        }
    }
}
fn reject(member: &mut CameraMemberResult, error: SourceError) {
    member.phase = CameraMemberPhase::Rejected;
    member.error = Some(error);
}
fn fail(member: &mut CameraMemberResult, error: SourceError) {
    member.phase = if error.kind == ErrorKind::Uncertain || error.transport_lost {
        CameraMemberPhase::Uncertain
    } else {
        CameraMemberPhase::Failed
    };
    member.error = Some(error);
}
fn emit(publish: &watch::Sender<CameraGroupResult>, report: &mut CameraGroupResult) {
    report.sequence += 1;
    publish.send_replace(report.clone());
}
fn interruption(cancel: &CancellationToken, deadline: Instant) -> Option<CameraGroupPhase> {
    if cancel.is_cancelled() {
        Some(CameraGroupPhase::Cancelled)
    } else if Instant::now() >= deadline {
        Some(CameraGroupPhase::Deadline)
    } else {
        None
    }
}
#[derive(Clone)]
pub struct CameraGroupOperation {
    status: watch::Receiver<CameraGroupResult>,
    cancel: CancellationToken,
    finished: watch::Receiver<bool>,
    images: Images,
}
impl CameraGroupOperation {
    pub fn status(&self) -> CameraGroupResult {
        self.status.borrow().clone()
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    /// Exact operation-owned image pin; a later camera capture cannot replace it.
    pub fn image(&self, source: Uuid) -> Result<Arc<CapturedImage>, SourceError> {
        let report = self.status.borrow();
        let index = report
            .members
            .iter()
            .position(|m| m.source == source)
            .ok_or_else(|| invalid("Camera is not a member of this operation"))?;
        self.images.lock().unwrap()[index]
            .clone()
            .ok_or_else(|| SourceError::new(ErrorKind::Unavailable, "Group image is not available"))
    }
    pub async fn changed(&mut self) -> Result<CameraGroupResult, SourceError> {
        self.status
            .changed()
            .await
            .map_err(|_| SourceError::uncertain())?;
        Ok(self.status.borrow_and_update().clone())
    }
    pub async fn completed(&mut self) -> Result<CameraGroupResult, SourceError> {
        loop {
            let report = self.status.borrow_and_update().clone();
            if report.phase.terminal() {
                return Ok(report);
            }
            self.status
                .changed()
                .await
                .map_err(|_| SourceError::uncertain())?;
        }
    }
    pub async fn settled(&mut self) -> Result<(), SourceError> {
        while !*self.finished.borrow_and_update() {
            self.finished
                .changed()
                .await
                .map_err(|_| SourceError::uncertain())?;
        }
        Ok(())
    }
}
