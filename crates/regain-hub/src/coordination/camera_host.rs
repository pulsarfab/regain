//! Revision-owned camera work, exact image pins and bounded reattachment.
use super::{
    CameraGroup, CameraGroupConfig, CameraGroupOperation, CameraGroupPhase, CameraGroupResult,
    CameraMemberRequest, GroupBinding, HostedGroupPhase,
};
use crate::{
    activity::{Activity, ActivityCounter},
    camera::acquisition::{CameraSupervisor, CapturedImage},
    config::HubConfig,
    readout::invalid,
    source::{ErrorKind, SourceError},
};
use regain_core::CancellationToken;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::watch,
    time::{Instant, sleep_until},
};
use uuid::Uuid;

/// Both coordination classes use the same connection/running/terminal phases.
pub type HostedCameraPhase = HostedGroupPhase;
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostedCameraStatus {
    pub host_instance: Uuid,
    pub configuration_revision: Uuid,
    pub operation: Uuid,
    pub group: Uuid,
    pub sequence: u64,
    pub phase: HostedCameraPhase,
    pub bindings: Vec<GroupBinding>,
    /// Configured identities, mapped to physical sources in the core result.
    pub requests: Vec<CameraMemberRequest>,
    pub result: Option<CameraGroupResult>,
    pub failed_source: Option<Uuid>,
    pub error: Option<SourceError>,
}
struct Definition {
    config: CameraGroupConfig,
    bindings: Vec<GroupBinding>,
    cameras: Vec<Arc<CameraSupervisor>>,
}
struct Entry {
    status: watch::Receiver<HostedCameraStatus>,
    done: watch::Receiver<bool>,
    cancel: CancellationToken,
    running: Mutex<Option<CameraGroupOperation>>,
}
#[derive(Default)]
struct State {
    closed: bool,
    entries: BTreeMap<Uuid, Arc<Entry>>,
}
pub(crate) struct CameraGroupCoordinator {
    definitions: BTreeMap<Uuid, Arc<Definition>>,
    activity: ActivityCounter,
    state: Mutex<State>,
}
impl CameraGroupCoordinator {
    pub(crate) fn new(
        config: &HubConfig,
        cameras: &BTreeMap<Uuid, Arc<CameraSupervisor>>,
        activity: ActivityCounter,
    ) -> Self {
        let definitions = config
            .camera_groups
            .iter()
            .map(|group| {
                let mut resolved = group.clone();
                let bindings = resolved
                    .members
                    .iter_mut()
                    .map(|source| {
                        let configured_source = *source;
                        *source = config
                            .physical_camera_source(*source)
                            .expect("Validated camera leaf");
                        GroupBinding {
                            configured_source,
                            physical_source: *source,
                        }
                    })
                    .collect();
                let cameras = resolved
                    .members
                    .iter()
                    .map(|source| cameras[source].clone())
                    .collect();
                (
                    group.id,
                    Arc::new(Definition {
                        config: resolved,
                        bindings,
                        cameras,
                    }),
                )
            })
            .collect();
        Self {
            definitions,
            activity,
            state: Mutex::new(State::default()),
        }
    }
    pub(crate) fn start(
        &self,
        host: Uuid,
        revision: Uuid,
        group: Uuid,
        requests: Vec<CameraMemberRequest>,
    ) -> Result<HostedCameraStatus, SourceError> {
        let definition = self.definitions.get(&group).ok_or_else(unknown)?.clone();
        if requests.len() != definition.bindings.len()
            || requests.iter().zip(&definition.bindings).any(|(r, b)| {
                r.source != b.configured_source
                    || !r.exposure.duration_seconds.is_finite()
                    || r.exposure.duration_seconds < 0.0
            })
        {
            return Err(invalid(
                "Camera requests do not match configured members or durations",
            ));
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| invalid("Camera group requires a host runtime"))?;
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(SourceError::new(
                ErrorKind::Disconnected,
                "Camera group host is stopped",
            ));
        }
        if let Some(entry) = state.entries.get(&group) {
            if !*entry.done.borrow() {
                return Err(SourceError::new(
                    ErrorKind::Busy,
                    "Camera group operation is already active",
                ));
            }
            let status = entry.status.borrow();
            if status.phase == HostedCameraPhase::Failed
                && status
                    .error
                    .as_ref()
                    .is_some_and(|e| e.kind == ErrorKind::Uncertain)
            {
                return Err(SourceError::uncertain());
            }
        }
        let initial = HostedCameraStatus {
            host_instance: host,
            configuration_revision: revision,
            operation: Uuid::new_v4(),
            group,
            sequence: 1,
            phase: HostedCameraPhase::Connecting,
            bindings: definition.bindings.clone(),
            requests,
            result: None,
            failed_source: None,
            error: None,
        };
        let (publish, status) = watch::channel(initial.clone());
        let (finish, done) = watch::channel(false);
        let cancel = CancellationToken::new();
        let entry = Arc::new(Entry {
            status,
            done,
            cancel: cancel.clone(),
            running: Mutex::new(None),
        });
        let guard = CompletionGuard {
            publish,
            finish,
            activity: Some(Activity::new(self.activity.clone())),
            cancel: cancel.clone(),
        };
        state.entries.insert(group, entry.clone());
        let origin = Instant::now();
        let deadline = origin + Duration::from_secs_f64(definition.config.timeout_seconds);
        let activity = self.activity.clone();
        runtime.spawn(async move {
            let guard = guard;
            if let Err(error) = run(
                &definition,
                &entry,
                &guard.publish,
                &cancel,
                origin,
                deadline,
                activity,
            )
            .await
            {
                update(&guard.publish, |s| {
                    s.phase = HostedCameraPhase::Failed;
                    s.error = Some(error);
                });
            }
        });
        Ok(initial)
    }
    pub(crate) fn status(
        &self,
        group: Uuid,
        operation: Option<Uuid>,
    ) -> Result<HostedCameraStatus, SourceError> {
        let entry = self.entry(group, operation)?;
        let status = entry.status.borrow().clone();
        Ok(status)
    }
    fn entry(&self, group: Uuid, operation: Option<Uuid>) -> Result<Arc<Entry>, SourceError> {
        if !self.definitions.contains_key(&group) {
            return Err(unknown());
        }
        let state = self.state.lock().unwrap();
        let entry = state.entries.get(&group).ok_or_else(|| {
            SourceError::new(
                ErrorKind::Unavailable,
                "No retained operation for this camera group",
            )
        })?;
        if operation.is_some_and(|id| id != entry.status.borrow().operation) {
            return Err(SourceError::new(
                ErrorKind::Unavailable,
                "Camera group operation has been retired",
            ));
        }
        Ok(entry.clone())
    }
    pub(crate) fn cancel(
        &self,
        group: Uuid,
        operation: Uuid,
    ) -> Result<HostedCameraStatus, SourceError> {
        let entry = self.entry(group, Some(operation))?;
        entry.cancel.cancel();
        let status = entry.status.borrow().clone();
        Ok(status)
    }
    pub(crate) fn image(
        &self,
        group: Uuid,
        operation: Uuid,
        source: Uuid,
        generation: Uuid,
        acquisition: Uuid,
    ) -> Result<Arc<CapturedImage>, SourceError> {
        let entry = self.entry(group, Some(operation))?;
        let running = entry.running.lock().unwrap();
        let image = running
            .as_ref()
            .ok_or_else(|| {
                SourceError::new(
                    ErrorKind::Unavailable,
                    "Camera group has not acquired images",
                )
            })?
            .image(source)?;
        if image.identity.generation != generation || image.identity.acquisition != acquisition {
            return Err(SourceError::new(
                ErrorKind::Unavailable,
                "Camera group image identity does not match",
            ));
        }
        Ok(image)
    }
    pub(crate) async fn stop(&self) {
        let entries = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            state.entries.values().cloned().collect::<Vec<_>>()
        };
        for entry in &entries {
            entry.cancel.cancel();
        }
        for entry in entries {
            let mut done = entry.done.clone();
            while !*done.borrow_and_update() {
                if done.changed().await.is_err() {
                    break;
                }
            }
        }
    }
}
impl Drop for CameraGroupCoordinator {
    fn drop(&mut self) {
        for entry in self.state.get_mut().unwrap().entries.values() {
            entry.cancel.cancel();
        }
    }
}
struct CompletionGuard {
    publish: watch::Sender<HostedCameraStatus>,
    finish: watch::Sender<bool>,
    activity: Option<Activity>,
    cancel: CancellationToken,
}
impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        if matches!(
            self.publish.borrow().phase,
            HostedCameraPhase::Connecting | HostedCameraPhase::Running
        ) {
            update(&self.publish, |s| {
                s.phase = HostedCameraPhase::Failed;
                s.error = Some(SourceError::uncertain());
            });
        }
        drop(self.activity.take());
        self.finish.send_replace(true);
    }
}
async fn run(
    definition: &Definition,
    entry: &Entry,
    publish: &watch::Sender<HostedCameraStatus>,
    cancel: &CancellationToken,
    origin: Instant,
    deadline: Instant,
    activity: ActivityCounter,
) -> Result<(), SourceError> {
    let mut sessions = Vec::new();
    for (binding, camera) in definition.bindings.iter().zip(&definition.cameras) {
        let result = tokio::select! {biased;
            _=cancel.cancelled()=>Err(HostedCameraPhase::Cancelled),
            _=sleep_until(deadline)=>Err(HostedCameraPhase::Deadline),
            result=camera.connect()=>Ok(result),
        };
        match result {
            Ok(Ok(session)) => sessions.push(Arc::new(session)),
            Ok(Err(error)) => {
                update(publish, |s| s.failed_source = Some(binding.physical_source));
                return Err(error);
            }
            Err(phase) => {
                update(publish, |s| s.phase = phase);
                return Ok(());
            }
        }
    }
    let requests = publish
        .borrow()
        .requests
        .iter()
        .zip(&definition.bindings)
        .map(|(r, b)| CameraMemberRequest {
            source: b.physical_source,
            exposure: r.exposure,
        })
        .collect();
    let group = CameraGroup::new(definition.config.clone(), sessions)?;
    let mut running = group.start_owned(
        requests,
        publish.borrow().operation,
        cancel.clone(),
        origin,
        deadline,
        Some(Activity::new(activity)),
    )?;
    *entry.running.lock().unwrap() = Some(running.clone());
    let mut result = running.status();
    loop {
        if result.phase.terminal() {
            running.settled().await?;
        }
        update(publish, |s| {
            s.phase = phase(result.phase);
            s.result = Some(result.clone());
        });
        if result.phase.terminal() {
            return Ok(());
        }
        result = running.changed().await?;
    }
}
fn update(
    publish: &watch::Sender<HostedCameraStatus>,
    change: impl FnOnce(&mut HostedCameraStatus),
) {
    publish.send_modify(|s| {
        s.sequence += 1;
        change(s);
    });
}
fn phase(value: CameraGroupPhase) -> HostedCameraPhase {
    match value {
        CameraGroupPhase::Preflight | CameraGroupPhase::Capturing => HostedCameraPhase::Running,
        CameraGroupPhase::Complete => HostedCameraPhase::Complete,
        CameraGroupPhase::PreflightFailed => HostedCameraPhase::PreflightFailed,
        CameraGroupPhase::PartialFailure => HostedCameraPhase::PartialFailure,
        CameraGroupPhase::Cancelled => HostedCameraPhase::Cancelled,
        CameraGroupPhase::Deadline => HostedCameraPhase::Deadline,
    }
}
fn unknown() -> SourceError {
    invalid("Unknown camera group")
}
