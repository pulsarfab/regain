//! Explicit private camera actors only; no installed drivers or equipment.
use super::*;
use regain_hub::coordination::{
    CameraCancellationPolicy as Cancel, CameraFailurePolicy as Failure, CameraGroup,
    CameraGroupConfig, CameraGroupOperation, CameraGroupPhase as GroupPhase,
    CameraMemberPhase as MemberPhase, CameraMemberRequest,
};

async fn group(
    a: &Fixture,
    b: &Fixture,
    failure: Failure,
    cancellation: Cancel,
) -> (Arc<CameraGroup>, Vec<CameraMemberRequest>) {
    let members = vec![a.source.snapshot().source, b.source.snapshot().source];
    let requests = members
        .iter()
        .map(|source| CameraMemberRequest {
            source: *source,
            exposure: request(),
            require_scalar_image: false,
        })
        .collect();
    let config = CameraGroupConfig {
        id: Uuid::new_v4(),
        label: "[SIMULATION] paired cameras".into(),
        timeout_seconds: 10.0,
        failure_policy: failure,
        cancellation_policy: cancellation,
        members,
    };
    (
        CameraGroup::new(
            config,
            vec![Arc::new(a.session().await), Arc::new(b.session().await)],
        )
        .unwrap(),
        requests,
    )
}
async fn capturing(operation: &CameraGroupOperation) {
    for _ in 0..100 {
        settle().await;
        if operation
            .status()
            .members
            .iter()
            .all(|m| m.phase == MemberPhase::Exposing)
        {
            return;
        }
        tokio::time::advance(Duration::from_millis(1)).await;
    }
    panic!("Group did not expose: {:?}", operation.status());
}
async fn terminal(
    operation: &mut CameraGroupOperation,
) -> regain_hub::coordination::CameraGroupResult {
    for _ in 0..500 {
        settle().await;
        if operation.status().phase.terminal() {
            operation.settled().await.unwrap();
            return operation.status();
        }
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    panic!("Group did not finish: {:?}", operation.status());
}
#[tokio::test(start_paused = true)]
async fn camera_group_validates_all_members_before_any_start() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    b.device.set("exposuremax", json!(0.5));
    let (group, requests) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let mut operation = group.start(requests).unwrap();
    let result = terminal(&mut operation).await;
    assert_eq!(result.phase, GroupPhase::PreflightFailed);
    assert_eq!(result.members[1].phase, MemberPhase::Rejected);
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        0
    );
    assert_eq!(a.activity.active(), 0);
    assert_eq!(b.activity.active(), 0);
    assert_eq!(a.supervisor.status().phase, Phase::Idle);
}
#[tokio::test(start_paused = true)]
async fn camera_group_requires_abort_capability_before_start_only_for_explicit_abort_policy() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    b.device.set("canabortexposure", json!(false));
    let (g, r) = group(&a, &b, Failure::AbortStarted, Cancel::LeaveRunning).await;
    let mut op = g.start(r).unwrap();
    assert_eq!(terminal(&mut op).await.phase, GroupPhase::PreflightFailed);
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        0
    );
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    a.device.complete();
    b.device.complete();
    assert_eq!(terminal(&mut op).await.phase, GroupPhase::Complete);
}
#[tokio::test(start_paused = true)]
async fn camera_group_retains_separate_exact_images_and_measures_host_start_spread() {
    let a = Fixture::new(48);
    let b = Fixture::new(24);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let a_id = r[0].source;
    let b_id = r[1].source;
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    a.device.complete();
    a.ready().await;
    settle().await;
    let pinned = op.image(a_id).unwrap();
    let original = pinned.identity.acquisition;
    let next = a.session().await;
    let next_id = next.start(request()).await.unwrap();
    a.device.complete();
    a.ready().await;
    assert_ne!(original, next_id);
    assert_eq!(op.image(a_id).unwrap().identity.acquisition, original);
    assert!(Arc::ptr_eq(&pinned, &op.image(a_id).unwrap()));
    b.device.complete();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::Complete);
    assert!(result.start_skew_seconds.unwrap() >= 0.0);
    assert_eq!(
        result.members[0].image.as_ref().unwrap().acquisition,
        original
    );
    assert_eq!(op.image(b_id).unwrap().identity.source, b_id);
    assert_eq!(a.device.downloads.load(SeqCst), 2);
    assert_eq!(b.device.downloads.load(SeqCst), 1);
    let sequence = result.sequence;
    settle().await;
    assert_eq!(op.status().sequence, sequence);
}
#[tokio::test(start_paused = true)]
async fn camera_group_continues_healthy_member_after_uncertain_start_without_replay() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    a.device.uncertain_start.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let mut op = g.start(r).unwrap();
    for _ in 0..100 {
        settle().await;
        if b.device.starts.load(SeqCst) == 1 {
            break;
        }
    }
    b.device.complete();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, MemberPhase::Uncertain);
    assert_eq!(result.members[1].phase, MemberPhase::Complete);
    assert_eq!(a.device.starts.load(SeqCst), 1);
    assert_eq!(b.device.starts.load(SeqCst), 1);
    assert_eq!(
        a.device.aborts.load(SeqCst) + b.device.aborts.load(SeqCst),
        0
    );
    assert_eq!(a.activity.active(), 1);
}
#[tokio::test(start_paused = true)]
async fn camera_group_explicit_failure_policy_aborts_only_acknowledged_members() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    a.device.uncertain_start.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::AbortStarted, Cancel::LeaveRunning).await;
    let mut op = g.start(r).unwrap();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, MemberPhase::Uncertain);
    assert_eq!(result.members[1].phase, MemberPhase::Aborted);
    assert_eq!(a.device.aborts.load(SeqCst), 0);
    assert_eq!(b.device.aborts.load(SeqCst), 1);
    assert_eq!(b.activity.active(), 0);
}
#[tokio::test(start_paused = true)]
async fn camera_group_cancellation_waits_for_admitted_start_ack_then_aborts_exact_capture() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    a.device.hold_start.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::AbortStarted).await;
    let mut op = g.start(r).unwrap();
    for _ in 0..100 {
        settle().await;
        if a.device.starts.load(SeqCst) == 1 && b.device.starts.load(SeqCst) == 1 {
            break;
        }
    }
    assert_eq!(a.device.starts.load(SeqCst), 1);
    op.cancel();
    settle().await;
    assert!(!op.status().phase.terminal());
    assert_eq!(a.device.aborts.load(SeqCst), 0);
    a.device.release_start.notify_one();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::Cancelled);
    assert!(
        result
            .members
            .iter()
            .all(|m| m.phase == MemberPhase::Aborted)
    );
    assert_eq!(a.device.aborts.load(SeqCst), 1);
    assert_eq!(b.device.aborts.load(SeqCst), 1);
    assert_eq!(a.device.starts.load(SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn camera_group_cancellation_can_explicitly_leave_running_exposures() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    op.cancel();
    assert_eq!(terminal(&mut op).await.phase, GroupPhase::Cancelled);
    assert_eq!(
        a.device.aborts.load(SeqCst) + b.device.aborts.load(SeqCst),
        0
    );
    assert_eq!(a.supervisor.status().phase, Phase::Exposing);
    a.device.complete();
    b.device.complete();
    a.ready().await;
    b.ready().await;
    assert_eq!(op.status().phase, GroupPhase::Cancelled);
    assert!(op.status().members.iter().all(|m| m.image.is_none()));
}
#[tokio::test(start_paused = true)]
async fn camera_group_cancel_before_preflight_dispatches_nothing() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::AbortStarted).await;
    let mut op = g.start(r).unwrap();
    op.cancel();
    assert_eq!(terminal(&mut op).await.phase, GroupPhase::Cancelled);
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        0
    );
    assert_eq!(a.activity.active() + b.activity.active(), 0);
}
#[tokio::test(start_paused = true)]
async fn camera_group_dropped_waiter_keeps_owned_work_and_ordinary_capture_busy() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let op = g.start(r.clone()).unwrap();
    let mut retained = op.clone();
    capturing(&op).await;
    assert_eq!(g.start(r).err().unwrap().kind, ErrorKind::Busy);
    assert_eq!(
        a.session().await.start(request()).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    drop(op);
    a.device.complete();
    b.device.complete();
    assert_eq!(terminal(&mut retained).await.phase, GroupPhase::Complete);
    assert_eq!(
        a.device.aborts.load(SeqCst) + b.device.aborts.load(SeqCst),
        0
    );
}
#[tokio::test(start_paused = true)]
async fn camera_group_publishes_healthy_image_while_sibling_start_ack_is_held() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    a.device.hold_start.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let b_id = r[1].source;
    let mut op = g.start(r).unwrap();
    for _ in 0..100 {
        settle().await;
        if b.device.starts.load(SeqCst) == 1 {
            break;
        }
    }
    b.device.complete();
    b.ready().await;
    settle().await;
    assert_eq!(op.status().members[1].phase, MemberPhase::Complete);
    assert!(op.image(b_id).is_ok());
    assert_eq!(op.status().start_skew_seconds, None);
    a.device.release_start.notify_one();
    settle().await;
    a.device.complete();
    assert_eq!(terminal(&mut op).await.phase, GroupPhase::Complete);
}
#[tokio::test(start_paused = true)]
async fn camera_group_budget_failure_retains_healthy_image_and_per_member_error() {
    let a = Fixture::new(1);
    let b = Fixture::new(24);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let b_id = r[1].source;
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    a.device.complete();
    b.device.complete();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, MemberPhase::Uncertain);
    assert_eq!(result.members[1].phase, MemberPhase::Complete);
    assert!(op.image(b_id).is_ok());
}

#[tokio::test(start_paused = true)]
async fn camera_group_does_not_abort_a_later_capture_after_member_completion() {
    let a = Fixture::new(48);
    let b = Fixture::new(24);
    b.device.hold_start.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::AbortStarted).await;
    let mut op = g.start(r).unwrap();
    for _ in 0..100 {
        settle().await;
        if a.device.starts.load(SeqCst) == 1 && b.device.starts.load(SeqCst) == 1 {
            break;
        }
    }
    a.device.complete();
    a.ready().await;
    settle().await;
    assert_eq!(op.status().members[0].phase, MemberPhase::Complete);
    let later = a.session().await;
    let later_id = later.start(request()).await.unwrap();
    op.cancel();
    settle().await;
    b.device.release_start.notify_one();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::Cancelled);
    assert_eq!(result.members[0].phase, MemberPhase::Complete);
    assert_eq!(a.device.aborts.load(SeqCst), 0);
    assert_eq!(b.device.aborts.load(SeqCst), 1);
    assert_eq!(a.supervisor.status().acquisition, Some(later_id));
    a.device.complete();
    a.ready().await;
}
#[tokio::test(start_paused = true)]
async fn camera_group_preflight_reservations_prevent_overlapping_groups_and_settings() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    b.device.hold_prepare.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let (other, other_requests) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let mut op = g.start(r).unwrap();
    for _ in 0..100 {
        settle().await;
        if b.device.prepare_reads.load(SeqCst) > 0 {
            break;
        }
    }
    assert_eq!(b.device.prepare_reads.load(SeqCst), 1);
    assert_eq!(
        a.session().await.set(S::Gain(10)).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    let mut overlap = other.start(other_requests).unwrap();
    assert_eq!(
        terminal(&mut overlap).await.phase,
        GroupPhase::PreflightFailed
    );
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        0
    );
    op.cancel();
    b.device.release_prepare.notify_one();
    settle().await;
    assert_eq!(terminal(&mut op).await.phase, GroupPhase::Cancelled);
    settle().await;
    assert_eq!(a.activity.active() + b.activity.active(), 0);
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        0
    );
}
#[tokio::test(start_paused = true)]
async fn camera_group_live_geometry_change_during_all_member_preflight_prevents_burst() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    b.device.hold_prepare.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let mut op = g.start(r).unwrap();
    for _ in 0..100 {
        settle().await;
        if b.device.prepare_reads.load(SeqCst) > 0 {
            break;
        }
    }
    a.device.set("numx", json!(2));
    b.device.hold_prepare.store(false, SeqCst);
    b.device.release_prepare.notify_one();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::PreflightFailed);
    assert_eq!(result.members[0].phase, MemberPhase::Rejected);
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        0
    );
}
#[tokio::test(start_paused = true)]
async fn camera_group_deadline_uses_explicit_abort_policy_without_reexposure() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    let (g, mut r) = group(&a, &b, Failure::Continue, Cancel::AbortStarted).await;
    for request in &mut r {
        request.exposure.duration_seconds = 10.0;
    }
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    tokio::time::advance(Duration::from_secs(10)).await;
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::Deadline);
    assert!(
        result
            .members
            .iter()
            .all(|m| m.phase == MemberPhase::Aborted)
    );
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        2
    );
    assert_eq!(
        a.device.aborts.load(SeqCst) + b.device.aborts.load(SeqCst),
        2
    );
}
#[tokio::test(start_paused = true)]
async fn camera_group_uncertain_abort_is_retained_without_retry() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    a.device.uncertain_abort.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::AbortStarted).await;
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    op.cancel();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::Cancelled);
    assert_eq!(result.members[0].phase, MemberPhase::Uncertain);
    assert_eq!(
        result.members[0].abort_error.as_ref().unwrap().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(result.members[1].phase, MemberPhase::Aborted);
    assert_eq!(a.device.aborts.load(SeqCst), 1);
    assert_eq!(a.activity.active(), 1);
}
#[tokio::test(start_paused = true)]
async fn camera_group_generation_loss_rejects_only_affected_image() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    a.device.hold_download.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let a_id = r[0].source;
    let b_id = r[1].source;
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    let generation = op.status().members[0].generation;
    a.device.complete();
    b.device.complete();
    a.phase(Phase::Downloading).await;
    a.device.errors.lock().unwrap().insert(
        "lastexposurestarttime".into(),
        SourceError {
            transport_lost: true,
            ..SourceError::new(ErrorKind::Disconnected, "Private group generation lost")
        },
    );
    a.device.release_download.notify_one();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::PartialFailure);
    assert_eq!(result.members[0].phase, MemberPhase::Uncertain);
    assert_eq!(result.members[0].generation, generation);
    assert_ne!(a.source.snapshot().generation, generation);
    assert!(op.image(a_id).is_err());
    assert!(op.image(b_id).is_ok());
    assert_eq!(a.budget.used_bytes(), 0);
    assert_eq!(a.device.starts.load(SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn camera_group_request_identity_and_invalid_duration_are_inert() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let mut reversed = r.clone();
    reversed.reverse();
    assert_eq!(
        g.start(reversed).err().unwrap().kind,
        ErrorKind::InvalidValue
    );
    for invalid in [f64::NAN, f64::INFINITY, -1.0] {
        let mut invalid_requests = r.clone();
        invalid_requests[1].exposure.duration_seconds = invalid;
        assert_eq!(
            g.start(invalid_requests).err().unwrap().kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(a.activity.active() + b.activity.active(), 0);
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        0
    );
}

#[tokio::test(start_paused = true)]
async fn camera_group_cancel_during_readout_retains_image_when_abort_is_rejected() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    a.device.hold_download.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::AbortStarted).await;
    let a_id = r[0].source;
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    a.device.complete();
    a.phase(Phase::Downloading).await;
    settle().await;
    op.cancel();
    settle().await;
    assert!(!op.status().phase.terminal());
    assert_eq!(a.device.aborts.load(SeqCst), 0);
    assert_eq!(b.device.aborts.load(SeqCst), 1);
    a.device.release_download.notify_one();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::Cancelled);
    assert_eq!(result.members[0].phase, MemberPhase::Complete);
    assert_eq!(
        result.members[0].abort_error.as_ref().unwrap().kind,
        ErrorKind::Busy
    );
    assert!(op.image(a_id).is_ok());
}
#[tokio::test(start_paused = true)]
async fn camera_group_host_dispatch_skew_is_distinct_from_acknowledgement_delay() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    a.device.hold_start.store(true, SeqCst);
    let (g, r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    let mut op = g.start(r).unwrap();
    for _ in 0..100 {
        settle().await;
        if a.device.starts.load(SeqCst) == 1 && b.device.starts.load(SeqCst) == 1 {
            break;
        }
    }
    tokio::time::advance(Duration::from_millis(250)).await;
    a.device.release_start.notify_one();
    capturing(&op).await;
    let result = op.status();
    assert_eq!(result.start_skew_seconds, Some(0.0));
    assert!(
        result.members[0].acknowledgement_seconds.unwrap()
            - result.members[0].dispatch_seconds.unwrap()
            >= 0.25
    );
    a.device.complete();
    b.device.complete();
    assert_eq!(terminal(&mut op).await.phase, GroupPhase::Complete);
}
#[test]
fn camera_group_config_rejects_duplicates_unbounded_counts_and_nonfinite_deadlines() {
    let config = CameraGroupConfig {
        id: Uuid::new_v4(),
        label: "group".into(),
        timeout_seconds: 30.0,
        failure_policy: Failure::Continue,
        cancellation_policy: Cancel::LeaveRunning,
        members: vec![Uuid::new_v4(), Uuid::new_v4()],
    };
    config.validate().unwrap();
    for seconds in [f64::NAN, f64::INFINITY, 0.0, 604801.0] {
        let mut bad = config.clone();
        bad.timeout_seconds = seconds;
        assert!(bad.validate().is_err());
    }
    let mut bad = config.clone();
    bad.members[1] = bad.members[0];
    assert!(bad.validate().is_err());
    bad = config.clone();
    bad.members.truncate(1);
    assert!(bad.validate().is_err());
    bad = config.clone();
    bad.members = (0..33).map(|_| Uuid::new_v4()).collect();
    assert!(bad.validate().is_err());
    bad = config.clone();
    bad.members[1] = Uuid::nil();
    assert!(bad.validate().is_err());
    bad = config;
    bad.label = " ".into();
    assert!(bad.validate().is_err());
}

fn scalar_metadata(device: &Device, sensor: i32) {
    device.set("maxadu", json!(65535));
    device.set("sensortype", json!(sensor));
    device.set("bayeroffsetx", json!(1));
    device.set("bayeroffsety", json!(0));
    device.set("sensorname", json!("[SIMULATION] sensor"));
}
#[tokio::test(start_paused = true)]
async fn camera_group_scalar_profile_is_frozen_before_start_and_survives_later_changes() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    scalar_metadata(&a.device, 0);
    scalar_metadata(&b.device, 2);
    let (g, mut r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    for request in &mut r {
        request.require_scalar_image = true;
    }
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    let admitted = op.status();
    assert_eq!(
        admitted.members[0]
            .capture_profile
            .as_ref()
            .unwrap()
            .bayer_offset_x,
        0
    );
    assert_eq!(
        admitted.members[1]
            .capture_profile
            .as_ref()
            .unwrap()
            .bayer_offset_x,
        1
    );
    a.device.complete();
    b.device.complete();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::Complete);
    a.device.set("maxadu", json!(255));
    b.device.set("sensortype", json!(0));
    assert_eq!(
        op.status().members[0]
            .capture_profile
            .as_ref()
            .unwrap()
            .max_adu,
        65535
    );
    assert_eq!(
        op.status().members[1]
            .capture_profile
            .as_ref()
            .unwrap()
            .sensor_type,
        2
    );
    assert_eq!(
        result.members[0].capture_profile,
        admitted.members[0].capture_profile
    );
    assert!(op.image(a.source.snapshot().source).is_ok());
}
#[tokio::test(start_paused = true)]
async fn camera_group_scalar_profile_rejects_unsupported_or_malformed_members_before_any_start() {
    for (key, value) in [
        ("maxadu", json!(0)),
        ("sensortype", json!(1)),
        ("sensortype", json!(99)),
        ("bayeroffsetx", json!(-1)),
        ("sensorname", json!(123)),
    ] {
        let a = Fixture::new(24);
        let b = Fixture::new(24);
        scalar_metadata(&a.device, 0);
        scalar_metadata(&b.device, 2);
        b.device.set(key, value);
        let (g, mut r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
        for request in &mut r {
            request.require_scalar_image = true;
        }
        let mut op = g.start(r).unwrap();
        let result = terminal(&mut op).await;
        assert_eq!(result.phase, GroupPhase::PreflightFailed, "{key}");
        assert!(result.members[1].error.is_some());
        assert_eq!(
            a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
            0
        );
        assert_eq!(a.activity.active() + b.activity.active(), 0);
    }
}
#[tokio::test(start_paused = true)]
async fn camera_group_scalar_profile_recheck_prevents_a_burst_after_format_changes() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    scalar_metadata(&a.device, 0);
    scalar_metadata(&b.device, 2);
    let (g, mut r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    for request in &mut r {
        request.require_scalar_image = true;
    }
    b.device.hold_prepare.store(true, SeqCst);
    let mut op = g.start(r).unwrap();
    for _ in 0..100 {
        settle().await;
        if b.device.prepare_reads.load(SeqCst) > 0 {
            break;
        }
    }
    assert_eq!(b.device.prepare_reads.load(SeqCst), 1);
    a.device.set("maxadu", json!(255));
    b.device.hold_prepare.store(false, SeqCst);
    b.device.release_prepare.notify_one();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::PreflightFailed);
    assert_eq!(
        a.device.starts.load(SeqCst) + b.device.starts.load(SeqCst),
        0
    );
    assert_eq!(
        result.members[0].capture_profile.as_ref().unwrap().max_adu,
        65535
    );
}
#[tokio::test(start_paused = true)]
async fn camera_group_scalar_profile_missing_requirement_is_opt_in_and_sensor_name_is_optional() {
    let a = Fixture::new(24);
    let b = Fixture::new(24);
    let (g, mut r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    for request in &mut r {
        request.require_scalar_image = true;
    }
    let mut op = g.start(r).unwrap();
    assert_eq!(terminal(&mut op).await.phase, GroupPhase::PreflightFailed);
    scalar_metadata(&a.device, 0);
    scalar_metadata(&b.device, 0);
    for device in [&a.device, &b.device] {
        device.errors.lock().unwrap().insert(
            "sensorname".into(),
            SourceError::new(ErrorKind::Unsupported, "Unsupported sensor name"),
        );
    }
    let (g, mut r) = group(&a, &b, Failure::Continue, Cancel::LeaveRunning).await;
    for request in &mut r {
        request.require_scalar_image = true;
    }
    let mut op = g.start(r).unwrap();
    capturing(&op).await;
    a.device.complete();
    b.device.complete();
    let result = terminal(&mut op).await;
    assert_eq!(result.phase, GroupPhase::Complete);
    assert!(
        result
            .members
            .iter()
            .all(|m| m.capture_profile.as_ref().unwrap().sensor_name.is_none())
    );
}
