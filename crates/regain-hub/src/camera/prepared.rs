//! Preflight reservations and exact completed-image pins for explicit groups.
//! Uses the ordinary supervisor admission/control/completion path throughout.
use super::*;

pub(crate) struct PreparedCameraStart {
    supervisor: Arc<CameraSupervisor>,
    source: Arc<TypedSourceSession>,
    plan: StartPlan,
    request: ExposureRequest,
    operation: Option<Arc<SourceLease>>,
    geometry: Option<CaptureGeometry>,
    armed: bool,
    dispatched: bool,
    cleanup_started: bool,
}
pub(crate) struct CameraDispatch {
    pub acquisition: Uuid,
    pub started: oneshot::Receiver<CameraStartReceipt>,
    pub completed: oneshot::Receiver<Result<Arc<CapturedImage>, SourceError>>,
}
impl CameraSession {
    pub(crate) fn source_id(&self) -> Uuid {
        self.source.source_id()
    }
    pub(crate) fn generation(&self) -> Uuid {
        self.source.generation()
    }
    pub(crate) async fn prepare_group(
        &self,
        request: ExposureRequest,
    ) -> Result<PreparedCameraStart, SourceError> {
        let plan = self
            .supervisor
            .admit_start(&self.source, self.id, request)?;
        let mut reservation = PreparedCameraStart {
            supervisor: self.supervisor.clone(),
            source: self.source.clone(),
            plan,
            request,
            operation: None,
            geometry: None,
            armed: true,
            dispatched: false,
            cleanup_started: false,
        };
        let (reply, response) = oneshot::channel();
        tokio::spawn(async move {
            if reply.is_closed() {
                let _ = reservation.release(None).await;
                return;
            }
            match reservation
                .supervisor
                .prepare_start(&reservation.source, plan.id, request)
                .await
            {
                Ok((operation, geometry)) => {
                    reservation.operation = Some(operation);
                    reservation.geometry = Some(geometry);
                    // A closed receiver drops the reservation and queues owned cleanup.
                    let _ = reply.send(Ok(reservation));
                }
                Err(mut error) => {
                    if let Err(cleanup) = reservation.release(Some(error.clone())).await {
                        error = cleanup;
                    }
                    let _ = reply.send(Err(error));
                }
            }
        });
        response.await.map_err(|_| SourceError::uncertain())?
    }
    /// Check the exact acquisition under the supervisor's command-admission lock.
    /// A completed capture must never cause an abort of a later sibling capture.
    pub(crate) async fn abort_group(&self, acquisition: Uuid) -> Result<(), SourceError> {
        self.supervisor
            .command(self.source.clone(), self.id, true, Some(acquisition))
            .await
    }
}
impl PreparedCameraStart {
    pub(crate) async fn recheck(&self, require_abort: bool) -> Result<(), SourceError> {
        let geometry = prepare(&self.source, self.request).await?;
        if Some(geometry) != self.geometry {
            return Err(invalid("Camera geometry changed after group preflight"));
        }
        if require_abort {
            command_capability(&self.source, true).await?;
        }
        Ok(())
    }
    pub(crate) async fn release(mut self, error: Option<SourceError>) -> Result<(), SourceError> {
        self.cleanup_started = true;
        let result = self.supervisor.end_rejected(self.plan.id, error).await;
        self.armed = false;
        result
    }
    /// Launches once; dropping either receiver never cancels or replays the work.
    pub(crate) fn dispatch(mut self) -> CameraDispatch {
        let (started, acknowledgement) = oneshot::channel();
        let (completed, image) = oneshot::channel();
        let acquisition = self.plan.id;
        tokio::spawn(async move {
            self.dispatched = true;
            self.supervisor
                .clone()
                .dispatch_start(
                    self.source.clone(),
                    self.plan,
                    self.request,
                    self.operation
                        .as_ref()
                        .expect("Prepared camera control")
                        .clone(),
                    self.geometry.expect("Prepared geometry"),
                    StartReply::Group { started, completed },
                )
                .await;
            self.armed = false;
            // Capture the whole Drop guard, not disjoint fields in this closure.
            drop(self);
        });
        CameraDispatch {
            acquisition,
            started: acknowledgement,
            completed: image,
        }
    }
}
struct CleanupGuard {
    supervisor: Arc<CameraSupervisor>,
    id: Uuid,
    finished: bool,
}
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.supervisor.uncertain(self.id, SourceError::uncertain());
        }
    }
}
impl Drop for PreparedCameraStart {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.dispatched || self.cleanup_started {
            self.supervisor
                .uncertain(self.plan.id, SourceError::uncertain());
        } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let mut guard = CleanupGuard {
                supervisor: self.supervisor.clone(),
                id: self.plan.id,
                finished: false,
            };
            runtime.spawn(async move {
                let _ = guard.supervisor.end_rejected(guard.id, None).await;
                guard.finished = true;
                // Keep the Drop guard intact through the owned cleanup task.
                drop(guard);
            });
        } else {
            self.supervisor
                .uncertain(self.plan.id, SourceError::uncertain());
        }
    }
}
