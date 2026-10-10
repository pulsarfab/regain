use super::*;

#[tokio::test(start_paused = true)]
async fn resume_during_start_is_uncertain_and_never_replays_or_aborts() {
    let f = Fixture::new(24);
    let owner = Arc::new(f.session().await);
    f.device.hold_start.store(true, SeqCst);
    let start = tokio::spawn({
        let owner = owner.clone();
        async move { owner.start(request()).await }
    });
    settle().await;
    assert_eq!(f.device.starts.load(SeqCst), 1);
    f.clock.notify_resume();
    assert_eq!(f.supervisor.status().phase, Phase::Uncertain);
    settle().await;
    assert!(start.is_finished());
    assert_eq!(start.await.unwrap().unwrap_err().kind, ErrorKind::Uncertain);
    f.device.release_start.notify_one();
    settle().await;
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    assert_eq!(f.device.stops.load(SeqCst), 0);
    assert!(!f.supervisor.status().image_ready);
    assert!(f.source.snapshot().write_uncertain);
    owner.abandon_uncertain().unwrap();
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn resume_wakes_a_long_exposure_poll_and_preserves_uncertain_ownership() {
    let f = Fixture::with_poll_interval(24, Duration::from_secs(60));
    let owner = f.session().await;
    owner.start(request()).await.unwrap();
    settle().await;
    let now = tokio::time::Instant::now();
    f.clock.notify_resume();
    assert_eq!(f.supervisor.status().phase, Phase::Uncertain);
    settle().await;
    assert_eq!(tokio::time::Instant::now(), now);
    assert_eq!(f.supervisor.status().phase, Phase::Uncertain);
    assert_eq!(f.activity.active(), 1);
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    assert_eq!(f.device.stops.load(SeqCst), 0);
    owner.abandon_uncertain().unwrap();
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn resume_discards_a_pending_download_without_publishing_a_late_image() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    owner.start(request()).await.unwrap();
    f.device.hold_download.store(true, SeqCst);
    f.device.complete();
    f.phase(Phase::Downloading).await;
    settle().await;
    assert_eq!(f.device.downloads.load(SeqCst), 1);
    f.clock.notify_resume();
    assert!(!f.supervisor.status().image_ready);
    settle().await;
    f.device.release_download.notify_one();
    settle().await;
    assert!(!f.supervisor.status().image_ready);
    assert!(owner.image().is_err());
    assert_eq!(f.supervisor.status().phase, Phase::Uncertain);
    assert_eq!(f.device.downloads.load(SeqCst), 1);
    assert_eq!(f.device.starts.load(SeqCst), 1);
    owner.abandon_uncertain().unwrap();
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn resume_hides_completed_image_but_does_not_corrupt_an_already_pinned_copy() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    owner.start(request()).await.unwrap();
    f.device.complete();
    f.ready().await;
    let pinned = owner.image().unwrap();
    assert!(f.supervisor.status().image_ready);
    f.clock.notify_resume();
    assert!(!f.supervisor.status().image_ready);
    assert!(owner.image().is_err());
    assert_eq!(pinned.image.bytes(), [17; 12]);
    settle().await;
    let fresh = f.session().await;
    assert!(fresh.image().is_err());
    assert_eq!(f.device.starts.load(SeqCst), 1);
    drop((owner, fresh, pinned));
    f.source.shutdown().await.unwrap();
}
