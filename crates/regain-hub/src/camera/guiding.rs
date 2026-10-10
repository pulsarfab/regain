//! Retained pulse guiding, independent of exposure/image lifetime. No implicit
//! stop, replay or claimed axis synchronization is added to an upstream camera.
use super::*;
pub(super) type GuideTask = Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuideRequest {
    #[schemars(range(min = 0, max = 3))]
    pub direction: i32,
    #[schemars(range(min = 0, max = 2147483647))]
    pub duration_milliseconds: i32,
}
impl GuideRequest {
    pub fn validate(self) -> Result<(), SourceError> {
        if !(0..=3).contains(&self.direction) || self.duration_milliseconds < 0 {
            return Err(invalid("Invalid camera guide direction or duration"));
        }
        Ok(())
    }
    pub fn from_parameters(args: &Values) -> Result<Self, SourceError> {
        let integer = |key| {
            args.get(key)
                .and_then(serde_json::Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
        };
        if args.len() != 2 {
            return Err(invalid("Expected guide Direction and Duration"));
        }
        let request = Self {
            direction: integer("Direction").ok_or_else(|| invalid("Expected guide Direction"))?,
            duration_milliseconds: integer("Duration")
                .ok_or_else(|| invalid("Expected guide Duration"))?,
        };
        request.validate()?;
        Ok(request)
    }
    pub(crate) fn parameters(self) -> Values {
        Values::from([
            ("Direction".into(), json!(self.direction)),
            ("Duration".into(), json!(self.duration_milliseconds)),
        ])
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum GuidingPhase {
    Starting,
    Guiding,
    Finishing,
    Uncertain,
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuidingStatus {
    pub id: Uuid,
    pub owner: Uuid,
    pub generation: Uuid,
    pub request: GuideRequest,
    pub phase: GuidingPhase,
    pub error: Option<SourceError>,
}
pub(super) struct GuideActive {
    pub(super) status: GuidingStatus,
    pub(super) operation: Option<Arc<SourceLease>>,
    _activity: Activity,
}
impl CameraSupervisor {
    pub(super) fn guide_retains(state: &State, operation: &Arc<SourceLease>) -> bool {
        state
            .guiding
            .as_ref()
            .and_then(|guide| guide.operation.as_ref())
            .is_some_and(|guide| Arc::ptr_eq(guide, operation))
    }
    fn guide_uncertain(&self, id: Uuid, error: SourceError) {
        if let Some(guide) = self
            .state
            .lock()
            .unwrap()
            .guiding
            .as_mut()
            .filter(|guide| guide.status.id == id)
        {
            guide.status.phase = GuidingPhase::Uncertain;
            guide.status.error = Some(error);
        }
        self.changed.notify_waiters();
    }
    async fn finish_guide(&self, id: Uuid) -> Result<(), SourceError> {
        let operation = {
            let mut state = self.state.lock().unwrap();
            let Some(guide) = state.guiding.as_ref().filter(|guide| guide.status.id == id) else {
                return Ok(());
            };
            let operation = guide.operation.clone();
            if operation.as_ref().is_some_and(|operation| {
                state
                    .active
                    .as_ref()
                    .and_then(|active| active.operation.as_ref())
                    .is_some_and(|active| Arc::ptr_eq(active, operation))
            }) || operation.is_none()
            {
                state.guiding = None;
                self.changed.notify_waiters();
                return Ok(());
            }
            state.guiding.as_mut().unwrap().status.phase = GuidingPhase::Finishing;
            operation.unwrap()
        };
        if let Err(error) = operation.source.control(operation.id, false).await {
            self.guide_uncertain(id, error.clone());
            return Err(error);
        }
        let mut state = self.state.lock().unwrap();
        if state
            .guiding
            .as_ref()
            .is_some_and(|guide| guide.status.id == id)
        {
            state.guiding = None;
        }
        self.changed.notify_waiters();
        Ok(())
    }
    async fn run_guide(
        self: Arc<Self>,
        source: Arc<TypedSourceSession>,
        id: Uuid,
        request: GuideRequest,
        completion_allowance: Duration,
        borrowed: Option<Arc<SourceLease>>,
        reply: oneshot::Sender<Result<Uuid, SourceError>>,
    ) {
        let admitted = timeout(self.timing.admission_timeout, async {
            if reply.is_closed() {
                return Ok(None);
            }
            let operation = match borrowed {
                Some(operation) => operation,
                None => Arc::new(source.operation().await?),
            };
            {
                let mut state = self.state.lock().unwrap();
                let guide = state
                    .guiding
                    .as_mut()
                    .filter(|guide| guide.status.id == id)
                    .ok_or_else(busy)?;
                guide.operation = Some(operation.clone());
            }
            if !boolean(&source, "canpulseguide").await? {
                return Err(SourceError::new(
                    ErrorKind::Unsupported,
                    "Camera does not support pulse guiding",
                ));
            }
            if boolean(&source, "ispulseguiding").await? {
                return Err(busy());
            }
            source.snapshot()?;
            Ok(Some(operation))
        })
        .await
        .unwrap_or_else(|_| Err(SourceError::timeout()));
        let operation = match admitted {
            Ok(Some(operation)) if !reply.is_closed() => operation,
            other => {
                let release = self.finish_guide(id).await;
                let result = release.and_then(|_| other.map(|_| id));
                let _ = reply.send(result);
                return;
            }
        };
        if let Err(mut error) = source
            .write(&operation, "pulseguide", request.parameters())
            .await
        {
            if error.kind == ErrorKind::Uncertain || error.transport_lost {
                self.guide_uncertain(id, error.clone());
            } else {
                if let Err(release) = self.finish_guide(id).await {
                    error = release;
                }
            }
            let _ = reply.send(Err(error));
            return;
        }
        let guiding = match boolean(&source, "ispulseguiding").await {
            Ok(guiding) => guiding,
            Err(mut error) => {
                error.kind = ErrorKind::Uncertain;
                self.guide_uncertain(id, error.clone());
                let _ = reply.send(Err(error));
                return;
            }
        };
        if !guiding {
            let result = self.finish_guide(id).await.map(|_| id);
            let _ = reply.send(result);
            return;
        }
        {
            let mut state = self.state.lock().unwrap();
            let Some(guide) = state.guiding.as_mut().filter(|guide| guide.status.id == id) else {
                return;
            };
            guide.status.phase = GuidingPhase::Guiding;
        }
        self.changed.notify_waiters();
        let _ = reply.send(Ok(id));
        let result = timeout(completion_allowance, async {
            loop {
                let next = tokio::time::sleep(self.timing.poll_interval);
                tokio::pin!(next);
                loop {
                    // Shutdown must wake long polls without unrelated changes
                    // repeatedly postponing the next completion observation.
                    let changed = self.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    if self.state.lock().unwrap().retired {
                        return Ok(());
                    }
                    tokio::select! {
                        _ = &mut next => break,
                        _ = &mut changed => {},
                    }
                }
                if !boolean(&source, "ispulseguiding").await? {
                    return Ok::<_, SourceError>(());
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(SourceError::new(
                ErrorKind::Uncertain,
                "Camera guide completion deadline expired",
            ))
        });
        match result {
            Ok(()) => {
                let _ = self.finish_guide(id).await;
            }
            Err(mut error) => {
                error.kind = ErrorKind::Uncertain;
                self.guide_uncertain(id, error);
            }
        }
    }
}
impl CameraSession {
    pub(super) fn guide_property_error(&self, property: CameraProperty) -> Result<(), SourceError> {
        if property == CameraProperty::IsPulseGuiding
            && let Some(error) = self
                .supervisor
                .state
                .lock()
                .unwrap()
                .guiding
                .as_ref()
                .and_then(|guide| guide.status.error.clone())
        {
            return Err(error);
        }
        Ok(())
    }
    /// Return after acknowledgement; retain ownership and observation through
    /// completion even if the waiter/client disappears. Same-owner exposures can
    /// overlap guiding; one guide pulse is admitted at a time across all outputs.
    pub async fn pulse_guide(&self, request: GuideRequest) -> Result<Uuid, SourceError> {
        request.validate()?;
        let completion_allowance = Duration::from_millis(request.duration_milliseconds as u64)
            .checked_add(self.supervisor.timing.readiness_grace)
            .filter(|duration| Instant::now().checked_add(*duration).is_some())
            .ok_or_else(|| invalid("Camera guide deadline is not representable"))?;
        if self.supervisor.source.snapshot().write_uncertain {
            return Err(SourceError::uncertain());
        }
        self.source.snapshot()?;
        let id = Uuid::new_v4();
        let (reply, response) = oneshot::channel();
        {
            let mut state = self.supervisor.state.lock().unwrap();
            if state.retired {
                return Err(SourceError::new(
                    ErrorKind::Disconnected,
                    "Camera runtime has retired",
                ));
            }
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
            let borrowed = match &state.active {
                None => None,
                Some(active) if active.phase == AcquisitionPhase::Uncertain => {
                    return Err(SourceError::uncertain());
                }
                Some(active)
                    if active.owner == self.id
                        && !active.command_pending
                        && matches!(
                            active.phase,
                            AcquisitionPhase::Exposing | AcquisitionPhase::Reading
                        ) =>
                {
                    active.operation.clone()
                }
                Some(_) => return Err(busy()),
            };
            state.guiding = Some(GuideActive {
                status: GuidingStatus {
                    id,
                    owner: self.id,
                    generation: self.source.generation(),
                    request,
                    phase: GuidingPhase::Starting,
                    error: None,
                },
                operation: borrowed.clone(),
                _activity: Activity::new(self.supervisor.activity.clone()),
            });
            // Register under the same state lock as retirement. Shutdown takes
            // and joins every guide task after the source actor has drained.
            let mut tasks = self.supervisor.guiding_tasks.lock().unwrap();
            tasks.retain(|task| {
                task.try_lock().map_or(true, |handle| {
                    handle.as_ref().is_some_and(|task| !task.is_finished())
                })
            });
            tasks.push(Arc::new(tokio::sync::Mutex::new(Some(tokio::spawn(
                self.supervisor.clone().run_guide(
                    self.source.clone(),
                    id,
                    request,
                    completion_allowance,
                    borrowed,
                    reply,
                ),
            )))));
        }
        response
            .await
            .map_err(|_| unavailable("Camera guide task stopped"))?
    }
    /// Abandon only local uncertain guide ownership. No pulse/stop or source
    /// uncertainty reset is sent; another owner's guide cannot be abandoned.
    pub fn abandon_guiding(&self) -> Result<(), SourceError> {
        let mut state = self.supervisor.state.lock().unwrap();
        let Some(guide) = &state.guiding else {
            return Ok(());
        };
        if guide.status.owner != self.id {
            return Err(busy());
        }
        if guide.status.phase != GuidingPhase::Uncertain {
            return Err(busy());
        }
        state.guiding = None;
        self.supervisor.changed.notify_waiters();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::DeviceType,
        parameters::PollPolicy,
        safety::MonotonicClock,
        simulated::{SimulatedBackend, SimulationUpdate},
        source::Backend,
    };

    #[tokio::test(start_paused = true)]
    async fn cached_false_cannot_hide_retained_guide_uncertainty_from_virtual_inputs_or_device_state()
     {
        let clock = Arc::new(MonotonicClock::default());
        let mut backend = SimulatedBackend::new(
            DeviceType::Camera,
            vec![crate::sampling::SampleRequest {
                key: "ispulseguiding".into(),
                member: "ispulseguiding".into(),
                parameters: Values::new(),
                value_type: crate::sampling::SampleType::Boolean,
                sensor_age: None,
            }],
        )
        .unwrap();
        backend
            .update_simulation(
                serde_json::from_value::<SimulationUpdate>(
                    json!({"camera":{"canPulseGuide":true}}),
                )
                .unwrap(),
            )
            .unwrap();
        let source = SourceHandle::spawn(
            Uuid::new_v4(),
            Uuid::new_v4(),
            PollPolicy::default(),
            Box::new(backend),
            clock.clone(),
        )
        .unwrap();
        let camera = CameraSupervisor::new(
            source.clone(),
            ImageBudget::new(1024).unwrap(),
            AcquisitionTiming {
                poll_interval: Duration::from_secs(60),
                readiness_grace: Duration::from_millis(10),
                ..Default::default()
            },
            ActivityCounter::default(),
        )
        .unwrap();
        let session = camera.connect().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !source.snapshot().values.contains_key("ispulseguiding") {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(source.snapshot().values["ispulseguiding"], json!(false));
        // Seed a valid raw false observation. Guiding later times out before its
        // first completion poll, even though the simulated physical pulse ends.
        session
            .pulse_guide(GuideRequest {
                direction: 0,
                duration_milliseconds: 1,
            })
            .await
            .unwrap();
        tokio::time::advance(Duration::from_millis(20)).await;
        for _ in 0..40 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            camera.status().guiding.unwrap().phase,
            GuidingPhase::Uncertain
        );
        assert_eq!(
            session
                .cached_sample(CameraProperty::IsPulseGuiding)
                .unwrap_err()
                .kind,
            ErrorKind::Uncertain
        );
        assert!(!session.device_state().contains_key("IsPulseGuiding"));
        source.shutdown().await.unwrap();
        camera.retire_after_source_shutdown().await;
    }

    #[tokio::test]
    async fn cancelled_retirement_keeps_the_handle_until_the_same_task_is_joined() {
        let source = SourceHandle::spawn(
            Uuid::new_v4(),
            Uuid::new_v4(),
            PollPolicy::default(),
            Box::new(SimulatedBackend::new(DeviceType::Camera, vec![]).unwrap()),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let camera = CameraSupervisor::new(
            source.clone(),
            ImageBudget::new(1024).unwrap(),
            AcquisitionTiming::default(),
            ActivityCounter::default(),
        )
        .unwrap();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let task = tokio::spawn({
            let entered = entered.clone();
            let release = release.clone();
            async move {
                entered.notify_one();
                release.notified().await;
            }
        });
        camera
            .guiding_tasks
            .lock()
            .unwrap()
            .push(Arc::new(tokio::sync::Mutex::new(Some(task))));
        entered.notified().await;
        source.shutdown().await.unwrap();
        {
            let retiring = camera.retire_after_source_shutdown();
            tokio::pin!(retiring);
            tokio::select! {biased; _=&mut retiring=>panic!("Retired before the retained task completed"),_ = tokio::task::yield_now()=>{}}
        }
        assert_eq!(camera.guiding_tasks.lock().unwrap().len(), 1);
        release.notify_one();
        camera.retire_after_source_shutdown().await;
        assert!(camera.guiding_tasks.lock().unwrap().is_empty());
    }
}
