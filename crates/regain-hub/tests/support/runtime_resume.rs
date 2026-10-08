use super::*;

#[tokio::test(start_paused = true)]
async fn resume_rejects_a_completed_reply_whose_waiter_has_not_consumed_it() {
    let f = fixture();
    let source = f.registry.get(f.config.sources[0].id).unwrap();
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    source.control(lease, true).await.unwrap();
    let mut write = Box::pin(source.write(lease, "fixture", Values::new()));
    assert!(futures_util::poll!(write.as_mut()).is_pending());
    settle().await;
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    f.clock.notify_resume();
    assert_eq!(write.await.unwrap_err().kind, ErrorKind::Uncertain);
    settle().await;
    let mut read = Box::pin(source.read(lease, "fixture", Values::new()));
    assert!(futures_util::poll!(read.as_mut()).is_pending());
    settle().await;
    assert_eq!(f.devices[0].reads.load(SeqCst), 1);
    f.clock.notify_resume();
    assert_eq!(read.await.unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    source.release(lease).await.unwrap();
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn resume_withdraws_safety_weather_and_switch_cache_before_tasks_run() {
    let f = fixture();
    let client = f.runtime.client();
    for output in [f.safety, f.weather, f.switch] {
        client.connect(output).await.unwrap();
    }
    settle().await;
    let safety = client.connection(f.safety).unwrap();
    let weather = client.connection(f.weather).unwrap();
    let switch = client.connection(f.switch).unwrap();
    assert!(safety.safety().unwrap().snapshot().is_safe);
    assert_eq!(
        weather
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .unwrap()
            .value,
        20.0
    );
    assert_eq!(switch.switch().unwrap().value(0).unwrap(), 1.0);
    let old: Vec<_> = f.runtime.source_snapshots();
    f.clock.notify_resume();
    // No yield: neither actors nor independent expiry consumers can run yet.
    assert!(!safety.safety().unwrap().snapshot().is_safe);
    assert!(
        weather
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .is_err()
    );
    assert_eq!(
        weather
            .weather()
            .unwrap()
            .time_since_last_update("temperature")
            .unwrap(),
        -1.0
    );
    assert!(switch.switch().unwrap().value(0).is_err());
    for (old, new) in old.iter().zip(f.runtime.source_snapshots()) {
        assert_ne!(old.generation, new.generation);
        assert_eq!(old.lease_count, new.lease_count);
        assert!(new.values.is_empty());
        assert!(!new.transport_connected);
    }
    assert_eq!(f.runtime.revision(), f.config.revision);
    assert!(client.connection(f.safety).is_ok());
    settle().await;
    assert!(safety.safety().unwrap().snapshot().is_safe);
    assert_eq!(
        weather
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .unwrap()
            .value,
        20.0
    );
    assert!(
        f.devices
            .iter()
            .all(|device| device.disconnects.load(SeqCst) == 0)
    );
    drop((safety, weather, switch));
    client.close();
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn resume_cancels_pending_read_without_waiting_for_its_timeout() {
    let f = fixture();
    let source = f.registry.get(f.config.sources[0].id).unwrap();
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    settle().await;
    f.devices[0].hang_read.store(true, SeqCst);
    let task = tokio::spawn({
        let source = source.clone();
        async move { source.read(lease, "fixture", Values::new()).await }
    });
    settle().await;
    assert_eq!(f.devices[0].reads.load(SeqCst), 1);
    let now = tokio::time::Instant::now();
    f.clock.notify_resume();
    settle().await;
    assert!(task.is_finished());
    assert_eq!(
        task.await.unwrap().unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(tokio::time::Instant::now(), now);
    source.release(lease).await.unwrap();
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn queued_pre_sleep_write_is_rejected_without_dispatch() {
    let f = fixture();
    let source = f.registry.get(f.config.sources[0].id).unwrap();
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    source.control(lease, true).await.unwrap();
    // Admit the queue entry, then sleep before the actor runs. This differs
    // from a dispatched write with an unknown outcome.
    let mut write = Box::pin(source.write(lease, "fixture", Values::new()));
    assert!(futures_util::poll!(write.as_mut()).is_pending());
    f.clock.notify_resume();
    assert_eq!(write.await.unwrap_err().kind, ErrorKind::Unavailable);
    assert_eq!(f.devices[0].writes.load(SeqCst), 0);
    assert!(!source.snapshot().write_uncertain);
    source.release(lease).await.unwrap();
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn interrupted_write_remains_uncertain_through_polling_and_another_resume() {
    let f = fixture();
    let source = f.registry.get(f.config.sources[0].id).unwrap();
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    source.control(lease, true).await.unwrap();
    f.devices[0].hang_write.store(true, SeqCst);
    let write = tokio::spawn({
        let source = source.clone();
        async move { source.write(lease, "fixture", Values::new()).await }
    });
    settle().await;
    assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    f.clock.notify_resume();
    settle().await;
    assert!(write.is_finished());
    assert_eq!(write.await.unwrap().unwrap_err().kind, ErrorKind::Uncertain);
    f.devices[0].hang_write.store(false, SeqCst);
    for _ in 0..2 {
        f.clock.notify_resume();
        settle().await;
        assert!(source.snapshot().write_uncertain);
        assert_eq!(
            source
                .write(lease, "fixture", Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.devices[0].writes.load(SeqCst), 1);
    }
    source.release(lease).await.unwrap();
    source.acquire(lease).await.unwrap();
    source.control(lease, true).await.unwrap();
    source.write(lease, "fixture", Values::new()).await.unwrap();
    assert_eq!(f.devices[0].writes.load(SeqCst), 2);
    source.release(lease).await.unwrap();
    f.runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn in_flight_safe_poll_cannot_restore_pre_sleep_permission() {
    let f = fixture();
    let client = f.runtime.client();
    client.connect(f.safety).await.unwrap();
    settle().await;
    let safety = client.connection(f.safety).unwrap();
    assert!(safety.safety().unwrap().snapshot().is_safe);
    f.devices[2].hang_poll.store(true, SeqCst);
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    let polls = f.devices[2].polls.load(SeqCst);
    f.clock.notify_resume();
    assert!(!safety.safety().unwrap().snapshot().is_safe);
    settle().await;
    assert!(!safety.safety().unwrap().snapshot().is_safe);
    assert!(f.devices[2].polls.load(SeqCst) > polls);
    assert_eq!(f.devices[2].disconnects.load(SeqCst), 0);
    f.devices[2].hang_poll.store(false, SeqCst);
    drop(safety);
    client.close();
    f.runtime.shutdown().await.unwrap();
}
