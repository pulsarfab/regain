//! Pulse guiding uses the acquisition fixtures, but owns its own retained work.
use super::*;
use regain_hub::camera::acquisition::{GuideRequest, GuidingPhase};

fn pulse(milliseconds: i32) -> GuideRequest {
    GuideRequest {
        direction: 2,
        duration_milliseconds: milliseconds,
    }
}
async fn reached(predicate: impl Fn() -> bool) {
    for _ in 0..100 {
        settle().await;
        if predicate() {
            return;
        }
        tokio::time::advance(Duration::from_millis(1)).await;
    }
    panic!("Guide fixture did not reach the requested barrier");
}
async fn guide_finished(f: &Fixture) {
    for _ in 0..100 {
        settle().await;
        if f.supervisor.status().guiding.is_none() {
            return;
        }
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    panic!("Guide not released: {:?}", f.supervisor.status());
}

#[tokio::test(start_paused = true)]
async fn invalid_and_unsupported_guides_never_dispatch_and_release_before_reply() {
    let f = Fixture::new(24);
    let session = f.session().await;
    let sibling = f.session().await;
    for (direction, duration) in [(-1, 0), (4, 1), (0, -1)] {
        assert_eq!(
            session
                .pulse_guide(GuideRequest {
                    direction,
                    duration_milliseconds: duration
                })
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    for args in [
        json!({"Direction":true,"Duration":1}),
        json!({"Direction":1.0,"Duration":1}),
        json!({"Direction":1,"Duration":"1"}),
        json!({"Direction":1,"Duration":2147483648u64}),
        json!({"Direction":1,"Duration":1,"extra":0}),
        json!({"Direction":1}),
    ] {
        assert!(GuideRequest::from_parameters(&serde_json::from_value(args).unwrap()).is_err());
    }
    assert!(pulse(i32::MAX).validate().is_ok());
    f.device.set("canpulseguide", json!(false));
    assert_eq!(
        session.pulse_guide(pulse(1)).await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(f.device.guides.load(SeqCst), 0);
    assert!(session.status().guiding.is_none());
    sibling.set(S::Gain(1)).await.unwrap();
    assert_eq!(f.activity.active(), 0);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn short_and_legacy_blocking_pulses_can_finish_before_the_first_status_read() {
    let f = Fixture::new(24);
    let session = f.session().await;
    let sibling = f.session().await;
    session.pulse_guide(pulse(0)).await.unwrap();
    assert!(session.status().guiding.is_none());
    sibling.set(S::Gain(1)).await.unwrap();
    f.device.blocking_guide.store(true, SeqCst);
    session.pulse_guide(pulse(500)).await.unwrap();
    assert!(session.status().guiding.is_none());
    sibling.set(S::Gain(2)).await.unwrap();
    assert_eq!(f.device.guides.load(SeqCst), 2);
    assert_eq!(f.activity.active(), 0);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn capture_finishes_first_and_guide_keeps_control_until_its_own_completion() {
    let f = Fixture::new(24);
    let session = f.session().await;
    let sibling = f.session().await;
    session.start(request()).await.unwrap();
    session.pulse_guide(pulse(1000)).await.unwrap();
    assert_eq!(f.activity.active(), 2);
    f.device.complete();
    f.ready().await;
    assert_eq!(session.image().unwrap().image.bytes().len(), 12);
    assert_eq!(session.status().phase, Phase::Idle);
    assert_eq!(
        session.status().guiding.unwrap().phase,
        GuidingPhase::Guiding
    );
    assert_eq!(f.activity.active(), 1);
    assert_eq!(
        sibling.start(request()).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        session.set(S::Gain(2)).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    f.device.set("ispulseguiding", json!(false));
    guide_finished(&f).await;
    sibling.set(S::Gain(2)).await.unwrap();
    assert_eq!(f.activity.active(), 0);
    assert_eq!(f.device.guides.load(SeqCst), 1);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn exposure_borrows_idle_guide_and_retains_control_after_guide_finishes() {
    let f = Fixture::new(24);
    let session = f.session().await;
    let sibling = f.session().await;
    session.pulse_guide(pulse(1000)).await.unwrap();
    session.start(request()).await.unwrap();
    assert_eq!(f.activity.active(), 2);
    f.device.set("ispulseguiding", json!(false));
    guide_finished(&f).await;
    assert_eq!(session.status().phase, Phase::Exposing);
    assert_eq!(f.activity.active(), 1);
    assert_eq!(
        sibling.pulse_guide(pulse(1)).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        sibling.set(S::Gain(2)).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    f.device.complete();
    f.ready().await;
    sibling.pulse_guide(pulse(0)).await.unwrap();
    assert_eq!(f.activity.active(), 0);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn abort_and_owner_disconnect_do_not_stop_a_retained_pulse() {
    let f = Fixture::new(24);
    let session = f.session().await;
    let sibling = f.session().await;
    session.start(request()).await.unwrap();
    session.pulse_guide(pulse(1000)).await.unwrap();
    session.abort().await.unwrap();
    assert_eq!(session.status().phase, Phase::Idle);
    assert!(session.status().guiding.is_some());
    drop(session);
    settle().await;
    assert_eq!(f.activity.active(), 1);
    assert_eq!(sibling.abandon_guiding().unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(
        sibling.pulse_guide(pulse(0)).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(f.device.aborts.load(SeqCst), 1);
    assert_eq!(f.device.stops.load(SeqCst), 0);
    assert_eq!(f.device.disconnects.load(SeqCst), 0);
    f.device.set("ispulseguiding", json!(false));
    guide_finished(&f).await;
    sibling.set(S::Gain(3)).await.unwrap();
    assert_eq!(f.activity.active(), 0);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn caller_cancellation_after_dispatch_keeps_pulse_and_blocks_competing_commands() {
    let f = Fixture::new(24);
    let owner = Arc::new(f.session().await);
    let sibling = f.session().await;
    f.device.hold_guide.store(true, SeqCst);
    let task = tokio::spawn({
        let owner = owner.clone();
        async move { owner.pulse_guide(pulse(1000)).await }
    });
    reached(|| f.device.guides.load(SeqCst) > 0).await;
    task.abort();
    let _ = task.await;
    assert_eq!(
        sibling.start(request()).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        owner.start(request()).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    f.device.release_guide.notify_one();
    settle().await;
    assert_eq!(owner.status().guiding.unwrap().phase, GuidingPhase::Guiding);
    drop(owner);
    assert_eq!(f.activity.active(), 1);
    f.device.set("ispulseguiding", json!(false));
    guide_finished(&f).await;
    sibling.set(S::Gain(3)).await.unwrap();
    assert_eq!(f.device.guides.load(SeqCst), 1);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_preflight_sends_no_pulse_and_releases_control() {
    let f = Fixture::new(24);
    let owner = Arc::new(f.session().await);
    let sibling = f.session().await;
    f.device.hold_guide_preflight.store(true, SeqCst);
    let task = tokio::spawn({
        let owner = owner.clone();
        async move { owner.pulse_guide(pulse(100)).await }
    });
    reached(|| f.device.guide_preflights.load(SeqCst) > 0).await;
    task.abort();
    let _ = task.await;
    f.device.release_guide_preflight.notify_one();
    guide_finished(&f).await;
    sibling.set(S::Gain(3)).await.unwrap();
    assert_eq!(f.device.guides.load(SeqCst), 0);
    assert_eq!(f.activity.active(), 0);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn lost_pulse_reply_fences_all_clients_and_local_abandon_does_not_clear_source_uncertainty() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    let sibling = f.session().await;
    f.device.uncertain_guide.store(true, SeqCst);
    assert_eq!(
        owner.pulse_guide(pulse(100)).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.status().guiding.unwrap().phase,
        GuidingPhase::Uncertain
    );
    assert_eq!(
        owner.property(P::IsPulseGuiding).await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    assert_eq!(
        sibling.set(S::Gain(3)).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(sibling.abandon_guiding().unwrap_err().kind, ErrorKind::Busy);
    owner.abandon_guiding().unwrap();
    assert!(f.source.snapshot().write_uncertain);
    assert_eq!(
        sibling.pulse_guide(pulse(0)).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        sibling.set(S::Gain(3)).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(f.device.guides.load(SeqCst), 1);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn completion_timeout_keeps_uncertainty_even_if_a_later_sample_says_false() {
    let f = Fixture::with_timing(24, Duration::from_millis(10), Duration::from_millis(100));
    let owner = f.session().await;
    owner.pulse_guide(pulse(1)).await.unwrap();
    tokio::time::advance(Duration::from_millis(120)).await;
    settle().await;
    assert_eq!(
        owner.status().guiding.unwrap().phase,
        GuidingPhase::Uncertain
    );
    f.device.set("ispulseguiding", json!(false));
    assert_eq!(
        owner.property(P::IsPulseGuiding).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.start(request()).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(f.activity.active(), 1);
    owner.abandon_guiding().unwrap();
    settle().await;
    owner.pulse_guide(pulse(0)).await.unwrap();
    assert_eq!(f.activity.active(), 0);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn guide_completion_read_failure_is_retained_after_upstream_recovers() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    owner.pulse_guide(pulse(1000)).await.unwrap();
    f.device.errors.lock().unwrap().insert(
        "ispulseguiding".into(),
        SourceError::new(ErrorKind::Unsupported, "Injected guide status failure"),
    );
    tokio::time::advance(Duration::from_millis(20)).await;
    settle().await;
    assert_eq!(
        owner.status().guiding.unwrap().phase,
        GuidingPhase::Uncertain
    );
    f.device.errors.lock().unwrap().clear();
    f.device.set("ispulseguiding", json!(false));
    assert_eq!(
        owner.property(P::IsPulseGuiding).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.pulse_guide(pulse(0)).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert!(!f.source.snapshot().write_uncertain);
    assert_eq!(f.device.guides.load(SeqCst), 1);
    owner.abandon_guiding().unwrap();
    settle().await;
    owner.pulse_guide(pulse(0)).await.unwrap();
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn rejected_exposure_releases_the_last_shared_lease_after_guide_completes() {
    let f = Fixture::new(24);
    let owner = Arc::new(f.session().await);
    let sibling = f.session().await;
    owner.pulse_guide(pulse(1000)).await.unwrap();
    f.device.hold_prepare.store(true, SeqCst);
    let task = tokio::spawn({
        let owner = owner.clone();
        async move { owner.start(request()).await }
    });
    reached(|| f.device.prepare_reads.load(SeqCst) > 0).await;
    f.device.set("ispulseguiding", json!(false));
    // The source actor is serial while prepare is held. Cancel the exposure
    // waiter, then let preflight and the pulse monitor finish in either order.
    task.abort();
    let _ = task.await;
    f.device.release_prepare.notify_one();
    guide_finished(&f).await;
    settle().await;
    sibling.set(S::Gain(3)).await.unwrap();
    assert_eq!(f.device.starts.load(SeqCst), 0);
    assert_eq!(f.activity.active(), 0);
    f.source.shutdown().await.unwrap();
}
