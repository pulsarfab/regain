//! Revision-owned group work and bounded reattachment inventory. Admission is
//! called under the runtime lifecycle lock, before configuration can quiesce.
use super::{
    FocuserGroup, FocuserGroupConfig, FocuserGroupPhase, FocuserGroupResult, interruptible,
};
use crate::{
    activity::{Activity, ActivityCounter},
    config::HubConfig,
    focuser::FocuserController,
    readout::invalid,
    source::{ErrorKind, SourceError, SourceRegistry},
};
use regain_core::CancellationToken;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, time::Instant};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HostedFocuserPhase {
    Connecting,
    Running,
    Complete,
    PreflightFailed,
    PartialFailure,
    Cancelled,
    Deadline,
    Failed,
}
impl HostedFocuserPhase {
    fn terminal(self) -> bool {
        !matches!(self, Self::Connecting | Self::Running)
    }
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserBinding {
    pub configured_source: Uuid,
    pub physical_source: Uuid,
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostedFocuserStatus {
    pub host_instance: Uuid,
    pub configuration_revision: Uuid,
    pub operation: Uuid,
    pub group: Uuid,
    pub logical_target: i32,
    pub sequence: u64,
    pub phase: HostedFocuserPhase,
    pub bindings: Vec<FocuserBinding>,
    pub result: Option<FocuserGroupResult>,
    pub failed_source: Option<Uuid>,
    pub error: Option<SourceError>,
}
struct Definition {
    config: FocuserGroupConfig,
    bindings: Vec<FocuserBinding>,
    controllers: Vec<Arc<FocuserController>>,
    activity: ActivityCounter,
}
struct Entry {
    status: watch::Receiver<HostedFocuserStatus>,
    done: watch::Receiver<bool>,
    cancel: CancellationToken,
}
#[derive(Default)]
struct State {
    closed: bool,
    // Exactly one latest entry per configured group, including terminal results.
    entries: BTreeMap<Uuid, Arc<Entry>>,
}
pub(crate) struct GroupCoordinator {
    definitions: BTreeMap<Uuid, Arc<Definition>>,
    activity: ActivityCounter,
    state: Mutex<State>,
}
impl GroupCoordinator {
    /// The runtime has validated both configuration and registry. No source I/O.
    pub(crate) fn new(
        config: &HubConfig,
        registry: &SourceRegistry,
        activity: ActivityCounter,
    ) -> Self {
        let definitions = config
            .focuser_groups
            .iter()
            .map(|group| {
                let mut resolved = group.clone();
                let mut bindings = Vec::new();
                let mut controllers = Vec::new();
                for member in &mut resolved.members {
                    let configured_source = member.source;
                    member.source = config
                        .physical_focuser_source(member.source)
                        .expect("Validated physical focuser leaf");
                    bindings.push(FocuserBinding {
                        configured_source,
                        physical_source: member.source,
                    });
                    let policy = &config
                        .sources
                        .iter()
                        .find(|source| source.id == member.source)
                        .expect("Validated source")
                        .polling;
                    controllers.push(Arc::new(
                        FocuserController::new(
                            registry.get(member.source).expect("Validated registry"),
                            Duration::from_secs_f64(policy.connection_timeout_seconds),
                        )
                        .expect("Validated connection deadline"),
                    ));
                }
                (
                    group.id,
                    Arc::new(Definition {
                        config: resolved,
                        bindings,
                        controllers,
                        activity: activity.clone(),
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
        target: i32,
    ) -> Result<HostedFocuserStatus, SourceError> {
        let definition = self
            .definitions
            .get(&group)
            .ok_or_else(unknown_group)?
            .clone();
        if !(definition.config.minimum..=definition.config.maximum).contains(&target) {
            return Err(invalid(
                "Focuser group logical target is outside configured travel",
            ));
        }
        for member in &definition.config.members {
            member.target(target)?;
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| invalid("Focuser group requires a host runtime"))?;
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Err(stopped());
        }
        if state
            .entries
            .get(&group)
            .is_some_and(|entry| !*entry.done.borrow())
        {
            return Err(SourceError::new(
                ErrorKind::Busy,
                "Focuser group operation is already active",
            ));
        }
        if state.entries.get(&group).is_some_and(|entry| {
            let status = entry.status.borrow();
            status.phase == HostedFocuserPhase::Failed
                && status
                    .error
                    .as_ref()
                    .is_some_and(|error| error.kind == ErrorKind::Uncertain)
        }) {
            return Err(SourceError::uncertain());
        }
        let initial = HostedFocuserStatus {
            host_instance: host,
            configuration_revision: revision,
            operation: Uuid::new_v4(),
            group,
            logical_target: target,
            sequence: 1,
            phase: HostedFocuserPhase::Connecting,
            bindings: definition.bindings.clone(),
            result: None,
            failed_source: None,
            error: None,
        };
        let (publish, status) = watch::channel(initial.clone());
        let (finish, done) = watch::channel(false);
        let cancel = CancellationToken::new();
        let guard = CompletionGuard {
            publish,
            finish,
            activity: Some(Activity::new(self.activity.clone())),
            cancel: cancel.clone(),
        };
        let entry = Arc::new(Entry {
            status,
            done,
            cancel: cancel.clone(),
        });
        state.entries.insert(group, entry);
        let deadline = Instant::now() + Duration::from_secs_f64(definition.config.timeout_seconds);
        runtime.spawn(async move {
            // Construct the guard before spawning, so abort/panic also publishes
            // an uncertain terminal result and retires the activity reservation.
            let guard = guard;
            if let Err(error) = run(
                &definition,
                target,
                initial.operation,
                &cancel,
                deadline,
                &guard.publish,
            )
            .await
            {
                update(&guard.publish, |status| {
                    status.phase = HostedFocuserPhase::Failed;
                    status.error = Some(error);
                });
            }
        });
        Ok(initial)
    }

    pub(crate) fn status(
        &self,
        group: Uuid,
        operation: Option<Uuid>,
    ) -> Result<HostedFocuserStatus, SourceError> {
        if !self.definitions.contains_key(&group) {
            return Err(unknown_group());
        }
        let state = self.state.lock().unwrap();
        let entry = state.entries.get(&group).ok_or_else(|| {
            SourceError::new(
                ErrorKind::Unavailable,
                "No retained operation for this focuser group",
            )
        })?;
        let status = entry.status.borrow().clone();
        if operation.is_some_and(|operation| operation != status.operation) {
            return Err(SourceError::new(
                ErrorKind::Unavailable,
                "Focuser group operation has been retired",
            ));
        }
        Ok(status)
    }
    pub(crate) fn cancel(
        &self,
        group: Uuid,
        operation: Uuid,
    ) -> Result<HostedFocuserStatus, SourceError> {
        let state = self.state.lock().unwrap();
        let entry = state.entries.get(&group).ok_or_else(unknown_group)?;
        let status = entry.status.borrow().clone();
        if operation != status.operation {
            return Err(invalid("Focuser group operation identity does not match"));
        }
        entry.cancel.cancel();
        Ok(status)
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
impl Drop for GroupCoordinator {
    fn drop(&mut self) {
        for entry in self.state.get_mut().unwrap().entries.values() {
            entry.cancel.cancel();
        }
    }
}
struct CompletionGuard {
    publish: watch::Sender<HostedFocuserStatus>,
    finish: watch::Sender<bool>,
    activity: Option<Activity>,
    cancel: CancellationToken,
}
impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        if !self.publish.borrow().phase.terminal() {
            update(&self.publish, |status| {
                status.phase = HostedFocuserPhase::Failed;
                status.error = Some(SourceError::uncertain());
            });
        }
        drop(self.activity.take());
        self.finish.send_replace(true);
    }
}
async fn run(
    definition: &Definition,
    target: i32,
    operation: Uuid,
    cancel: &CancellationToken,
    deadline: Instant,
    publish: &watch::Sender<HostedFocuserStatus>,
) -> Result<(), SourceError> {
    let mut sessions = Vec::new();
    for (binding, controller) in definition.bindings.iter().zip(&definition.controllers) {
        match interruptible(controller.connect(), cancel, deadline).await {
            Ok(Ok(session)) => sessions.push(Arc::new(session)),
            Ok(Err(error)) => {
                update(publish, |status| {
                    status.failed_source = Some(binding.physical_source);
                });
                return Err(error);
            }
            Err(reason) => {
                update(publish, |status| {
                    status.phase = phase(reason);
                });
                return Ok(());
            }
        }
    }
    let group = FocuserGroup::new(definition.config.clone(), sessions)?;
    let mut running = group.start_owned(
        target,
        operation,
        cancel.clone(),
        deadline,
        Some(Activity::new(definition.activity.clone())),
    )?;
    let mut result = running.status();
    loop {
        if result.phase.terminal() {
            running.settled().await?;
        }
        update(publish, |status| {
            status.phase = phase(result.phase);
            status.result = Some(result.clone());
        });
        if result.phase.terminal() {
            return Ok(());
        }
        result = running.changed().await?;
    }
}
fn update(
    publish: &watch::Sender<HostedFocuserStatus>,
    change: impl FnOnce(&mut HostedFocuserStatus),
) {
    publish.send_modify(|status| {
        status.sequence += 1;
        change(status);
    });
}
fn phase(phase: FocuserGroupPhase) -> HostedFocuserPhase {
    match phase {
        FocuserGroupPhase::Preflight | FocuserGroupPhase::Moving => HostedFocuserPhase::Running,
        FocuserGroupPhase::Complete => HostedFocuserPhase::Complete,
        FocuserGroupPhase::PreflightFailed => HostedFocuserPhase::PreflightFailed,
        FocuserGroupPhase::PartialFailure => HostedFocuserPhase::PartialFailure,
        FocuserGroupPhase::Cancelled => HostedFocuserPhase::Cancelled,
        FocuserGroupPhase::Deadline => HostedFocuserPhase::Deadline,
    }
}
fn unknown_group() -> SourceError {
    invalid("Unknown focuser group")
}
fn stopped() -> SourceError {
    SourceError::new(ErrorKind::Disconnected, "Focuser group host is stopped")
}
