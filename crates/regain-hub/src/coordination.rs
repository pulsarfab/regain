//! Explicit operations with independent member results. Standard single-device
//! interfaces remain single-device interfaces; coordination never claims rollback.
use crate::{
    focuser::FocuserSession,
    readout::invalid,
    source::{ErrorKind, SourceError},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use regain_core::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, future::Future, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, watch},
    time::{Instant, sleep, sleep_until},
};
use uuid::Uuid;
mod camera;
pub(crate) mod host;
pub use camera::*;
pub use host::{FocuserBinding, HostedFocuserPhase, HostedFocuserStatus};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserCalibration {
    /// Stable configured source identity, never an enumeration index. The host
    /// resolves virtual aliases to physical leaves before constructing a group.
    #[schemars(extend("x-regain" = {"reference":"source", "deviceType":"focuser"}))]
    pub source: Uuid,
    /// Signed scale numerator; negative values reverse logical motion.
    #[schemars(extend("default" = 1, "not" = {"const":0}))]
    #[schemars(range(min = -2147483648_i64, max = 2147483647_i64))]
    pub scale_numerator: i32,
    /// Positive scale denominator. Rounding is nearest, ties away from zero.
    #[schemars(range(min = 1, max = 2147483647_i64), extend("default" = 1))]
    pub scale_denominator: i32,
    /// Absolute device steps added after rounding the scaled logical target.
    #[schemars(range(min = -2147483648_i64, max = 2147483647_i64), extend("x-regain" = {"units":"steps"}))]
    pub offset: i32,
    /// Inclusive configured travel bounds, additionally restricted by live limits.
    #[schemars(range(min = 0, max = 2147483647_i64), extend("x-regain" = {"units":"steps"}))]
    pub minimum: i32,
    #[schemars(range(min = 0, max = 2147483647_i64), extend("x-regain" = {"units":"steps"}))]
    pub maximum: i32,
}
impl FocuserCalibration {
    fn validate(&self) -> Result<(), SourceError> {
        if self.source.is_nil()
            || self.scale_numerator == 0
            || self.scale_denominator <= 0
            || self.minimum < 0
            || self.maximum < self.minimum
        {
            return Err(invalid("Invalid focuser group calibration"));
        }
        Ok(())
    }
    pub fn target(&self, logical: i32) -> Result<i32, SourceError> {
        self.validate()?;
        let numerator = i64::from(logical) * i64::from(self.scale_numerator);
        let denominator = i64::from(self.scale_denominator);
        let mut scaled = numerator / denominator;
        if (numerator % denominator).abs() * 2 >= denominator {
            scaled += numerator.signum();
        }
        let target = i32::try_from(scaled + i64::from(self.offset))
            .map_err(|_| invalid("Focuser group target overflows device steps"))?;
        if !(self.minimum..=self.maximum).contains(&target) {
            return Err(invalid("Focuser group target is outside configured travel"));
        }
        Ok(target)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserGroupConfig {
    /// Stable group identity retained across edits and frontend reconnection.
    #[schemars(extend("readOnly" = true, "x-regain" = {"immutableAfterCreate":true}))]
    pub id: Uuid,
    /// Name shown in coordination controls; no standard Focuser output is created.
    #[schemars(length(min = 1, max = 200))]
    pub label: String,
    /// Inclusive logical coordinates admitted before any source I/O.
    #[schemars(range(min = -2147483648_i64, max = 2147483647_i64))]
    pub minimum: i32,
    #[schemars(range(min = -2147483648_i64, max = 2147483647_i64))]
    pub maximum: i32,
    /// Whole-operation bound, including reservations, preflight and completion.
    /// An in-flight mutation finishes its existing actor deadline before return.
    #[schemars(range(min = 0.01, max = 300.0), extend("default" = 120.0, "x-regain" = {"units":"s"}))]
    pub timeout_seconds: f64,
    /// Completion observation interval; does not change source polling policy.
    #[schemars(range(min = 0.01, max = 10.0), extend("default" = 0.1, "x-regain" = {"units":"s"}))]
    pub poll_seconds: f64,
    #[schemars(length(min = 2, max = 32))]
    pub members: Vec<FocuserCalibration>,
}
impl FocuserGroupConfig {
    pub fn validate(&self) -> Result<(), SourceError> {
        if self.id.is_nil()
            || self.label.trim().is_empty()
            || self.label.chars().count() > 200
            || self.maximum < self.minimum
            || !(2..=32).contains(&self.members.len())
            || !self.timeout_seconds.is_finite()
            || !(0.01..=300.0).contains(&self.timeout_seconds)
            || !self.poll_seconds.is_finite()
            || !(0.01..=10.0).contains(&self.poll_seconds)
        {
            return Err(invalid("Invalid focuser group configuration"));
        }
        let mut sources = BTreeSet::new();
        for member in &self.members {
            member.validate()?;
            if !sources.insert(member.source) {
                return Err(invalid("Focuser group repeats a physical source"));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FocuserMemberPhase {
    NotStarted,
    Rejected,
    Moving,
    Complete,
    Failed,
    Uncertain,
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserMemberResult {
    pub source: Uuid,
    pub generation: Uuid,
    pub target: i32,
    pub phase: FocuserMemberPhase,
    pub last_position: Option<i32>,
    pub error: Option<SourceError>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FocuserGroupPhase {
    Preflight,
    Moving,
    Complete,
    PreflightFailed,
    PartialFailure,
    Cancelled,
    Deadline,
}
impl FocuserGroupPhase {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Preflight | Self::Moving)
    }
}
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FocuserGroupResult {
    pub operation: Uuid,
    pub group: Uuid,
    pub logical_target: i32,
    /// Increases only on publication, never because a frontend reads status.
    pub sequence: u64,
    pub phase: FocuserGroupPhase,
    pub members: Vec<FocuserMemberResult>,
}

/// Constructed with already resolved, connected physical sessions. Resolving
/// virtual aliases and durable config/IPC admission belongs to the host layer.
/// A group never opens, reconnects, homes or changes compensation implicitly.
pub struct FocuserGroup {
    config: FocuserGroupConfig,
    sessions: Vec<Arc<FocuserSession>>,
    active: Arc<Mutex<()>>,
}
impl FocuserGroup {
    pub fn new(
        config: FocuserGroupConfig,
        sessions: Vec<Arc<FocuserSession>>,
    ) -> Result<Arc<Self>, SourceError> {
        config.validate()?;
        if sessions.len() != config.members.len()
            || sessions
                .iter()
                .zip(&config.members)
                .any(|(session, member)| session.source_id() != member.source)
        {
            return Err(invalid(
                "Focuser group sessions do not match physical sources",
            ));
        }
        Ok(Arc::new(Self {
            config,
            sessions,
            active: Arc::new(Mutex::new(())),
        }))
    }

    /// Owns work independently of the waiting frontend. Dropping a handle does
    /// not cancel or halt equipment. Explicit cancellation stops further work;
    /// already dispatched motion has no implied halt or rollback.
    pub fn start(
        self: &Arc<Self>,
        logical_target: i32,
    ) -> Result<FocuserGroupOperation, SourceError> {
        self.start_owned(
            logical_target,
            Uuid::new_v4(),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs_f64(self.config.timeout_seconds),
            None,
        )
    }
    pub(crate) fn start_owned(
        self: &Arc<Self>,
        logical_target: i32,
        operation: Uuid,
        cancel: CancellationToken,
        deadline: Instant,
        retained: Option<crate::activity::Activity>,
    ) -> Result<FocuserGroupOperation, SourceError> {
        if !(self.config.minimum..=self.config.maximum).contains(&logical_target) {
            return Err(invalid(
                "Focuser group logical target is outside configured travel",
            ));
        }
        let targets = self
            .config
            .members
            .iter()
            .map(|member| member.target(logical_target))
            .collect::<Result<Vec<_>, _>>()?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| invalid("Focuser group requires an active host runtime"))?;
        let active = self.active.clone().try_lock_owned().map_err(|_| {
            SourceError::new(ErrorKind::Busy, "Focuser group operation is already active")
        })?;
        let report = FocuserGroupResult {
            operation,
            group: self.config.id,
            logical_target,
            sequence: 1,
            phase: FocuserGroupPhase::Preflight,
            members: self
                .sessions
                .iter()
                .zip(targets)
                .map(|(session, target)| FocuserMemberResult {
                    source: session.source_id(),
                    generation: session.generation(),
                    target,
                    phase: FocuserMemberPhase::NotStarted,
                    last_position: None,
                    error: None,
                })
                .collect(),
        };
        let (publish, status) = watch::channel(report);
        let task_cancel = cancel.clone();
        let group = self.clone();
        let (finish, finished) = watch::channel(false);
        runtime.spawn(async move {
            group.run(publish, task_cancel, deadline).await;
            drop(active);
            drop(retained);
            finish.send_replace(true);
        });
        Ok(FocuserGroupOperation {
            status,
            cancel,
            finished,
        })
    }

    async fn run(
        &self,
        publish: watch::Sender<FocuserGroupResult>,
        cancel: CancellationToken,
        deadline: Instant,
    ) {
        let mut report = publish.borrow().clone();
        let mut reservations = Vec::with_capacity(self.sessions.len());
        // Every command lease is acquired before *any* physical write. Competing
        // groups fail Busy rather than wait in a lock-order cycle.
        for (index, session) in self.sessions.iter().enumerate() {
            match interruptible(session.reserve_motion(), &cancel, deadline).await {
                Ok(Ok(reservation)) => reservations.push(reservation),
                Ok(Err(error)) => {
                    reject(&mut report.members[index], error);
                    report.phase = FocuserGroupPhase::PreflightFailed;
                    emit(&publish, &mut report);
                    return;
                }
                Err(reason) => {
                    report.phase = reason;
                    emit(&publish, &mut report);
                    return;
                }
            }
        }
        for (index, reservation) in reservations.iter().enumerate() {
            match interruptible(
                reservation.preflight(report.members[index].target),
                &cancel,
                deadline,
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    reject(&mut report.members[index], error);
                    report.phase = FocuserGroupPhase::PreflightFailed;
                    emit(&publish, &mut report);
                    return;
                }
                Err(reason) => {
                    report.phase = reason;
                    emit(&publish, &mut report);
                    return;
                }
            }
        }
        report.phase = FocuserGroupPhase::Moving;
        emit(&publish, &mut report);
        for (index, reservation) in reservations.iter().enumerate() {
            // Recheck live limits immediately before each dispatch. Other
            // applications can still change equipment outside the hub leases.
            let preflight = interruptible(
                reservation.preflight(report.members[index].target),
                &cancel,
                deadline,
            )
            .await;
            match preflight {
                Err(reason) => {
                    report.phase = reason;
                    emit(&publish, &mut report);
                    return;
                }
                Ok(Err(error)) => {
                    reject(&mut report.members[index], error);
                    break;
                }
                Ok(Ok(())) => {}
            }
            if let Some(reason) = interrupted(&cancel, deadline) {
                report.phase = reason;
                emit(&publish, &mut report);
                return;
            }
            // Do not select cancellation against a mutation: losing its reply
            // would erase whether motion started. The actor already bounds it.
            match reservation.dispatch(report.members[index].target).await {
                Ok(()) => report.members[index].phase = FocuserMemberPhase::Moving,
                Err(error) => {
                    let member = &mut report.members[index];
                    member.phase = if error.kind == ErrorKind::Uncertain {
                        FocuserMemberPhase::Uncertain
                    } else {
                        FocuserMemberPhase::Failed
                    };
                    member.error = Some(error);
                    emit(&publish, &mut report);
                    break;
                }
            }
            emit(&publish, &mut report);
        }
        // Each member has an independent next observation. Completing one read
        // reschedules only that member; a stalled sibling cannot hold a round
        // barrier and prevent a healthy member's later completion publication.
        let mut observations = FuturesUnordered::new();
        for (index, member) in report.members.iter().enumerate() {
            if member.phase == FocuserMemberPhase::Moving {
                observations.push(observe(index, &self.sessions[index], Duration::ZERO));
            }
        }
        while !observations.is_empty() {
            let (index, observation) =
                match interruptible(observations.next(), &cancel, deadline).await {
                    Ok(Some(value)) => value,
                    Ok(None) => break,
                    Err(reason) => {
                        report.phase = reason;
                        emit(&publish, &mut report);
                        return;
                    }
                };
            let member = &mut report.members[index];
            match observation {
                Ok((moving, position)) => {
                    member.last_position = Some(position);
                    if !moving {
                        if position == member.target {
                            member.phase = FocuserMemberPhase::Complete;
                        } else {
                            member.phase = FocuserMemberPhase::Failed;
                            member.error = Some(SourceError::new(
                                ErrorKind::Unavailable,
                                "Focuser stopped before reaching its group target",
                            ));
                        }
                    }
                }
                Err(error) => {
                    member.phase = FocuserMemberPhase::Uncertain;
                    member.error = Some(error);
                }
            }
            if member.phase == FocuserMemberPhase::Moving {
                observations.push(observe(
                    index,
                    &self.sessions[index],
                    Duration::from_secs_f64(self.config.poll_seconds),
                ));
            }
            emit(&publish, &mut report);
        }
        report.phase = if report
            .members
            .iter()
            .all(|member| member.phase == FocuserMemberPhase::Complete)
        {
            FocuserGroupPhase::Complete
        } else {
            FocuserGroupPhase::PartialFailure
        };
        // A dispatched mutation may finish after cancellation/the group bound.
        // Preserve its member acknowledgement while reporting that stop reason.
        if let Some(reason) = interrupted(&cancel, deadline) {
            report.phase = reason;
        }
        emit(&publish, &mut report);
    }
}

#[derive(Clone)]
pub struct FocuserGroupOperation {
    status: watch::Receiver<FocuserGroupResult>,
    cancel: CancellationToken,
    finished: watch::Receiver<bool>,
}
impl FocuserGroupOperation {
    pub(crate) async fn settled(&mut self) -> Result<(), SourceError> {
        while !*self.finished.borrow_and_update() {
            self.finished
                .changed()
                .await
                .map_err(|_| SourceError::uncertain())?;
        }
        Ok(())
    }
    /// Local immutable observation: no I/O or mutation on a status read.
    pub fn status(&self) -> FocuserGroupResult {
        self.status.borrow().clone()
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    pub async fn changed(&mut self) -> Result<FocuserGroupResult, SourceError> {
        self.status
            .changed()
            .await
            .map_err(|_| SourceError::uncertain())?;
        Ok(self.status.borrow_and_update().clone())
    }
    pub async fn completed(&mut self) -> Result<FocuserGroupResult, SourceError> {
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
}
async fn observe(
    index: usize,
    session: &FocuserSession,
    delay: Duration,
) -> (usize, Result<(bool, i32), SourceError>) {
    if !delay.is_zero() {
        sleep(delay).await;
    }
    let result = async {
        let moving = session.is_moving().await?;
        let position = session.position().await?;
        Ok((moving, position))
    }
    .await;
    (index, result)
}

fn reject(member: &mut FocuserMemberResult, error: SourceError) {
    member.phase = FocuserMemberPhase::Rejected;
    member.error = Some(error);
}
fn emit(publish: &watch::Sender<FocuserGroupResult>, report: &mut FocuserGroupResult) {
    report.sequence += 1;
    publish.send_replace(report.clone());
}
fn interrupted(cancel: &CancellationToken, deadline: Instant) -> Option<FocuserGroupPhase> {
    if cancel.is_cancelled() {
        Some(FocuserGroupPhase::Cancelled)
    } else if Instant::now() >= deadline {
        Some(FocuserGroupPhase::Deadline)
    } else {
        None
    }
}
async fn interruptible<T>(
    work: impl Future<Output = T>,
    cancel: &CancellationToken,
    deadline: Instant,
) -> Result<T, FocuserGroupPhase> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(FocuserGroupPhase::Cancelled),
        _ = sleep_until(deadline) => Err(FocuserGroupPhase::Deadline),
        result = work => Ok(result),
    }
}
