use super::*;
use regain_hub::{
    config::ConfigStore,
    parameters::FieldError,
    service::{HubService, ServiceClient, UpdateError},
};
use std::path::PathBuf;

fn managed(f: &Fixture, path: Option<PathBuf>) -> Arc<HubService> {
    let devices = f.devices.clone();
    HubService::persistent(
        ConfigStore::new(path, f.config.clone()).unwrap(),
        Arc::new(move |mut config| {
            assert_ne!(
                config.outputs[0].label, "Panicking fixture",
                "Injected builder panic"
            );
            if config.outputs[0].label == "Unsupported fixture" {
                return Err(vec![FieldError::new(
                    "outputs[0]",
                    "unsupported",
                    "Fixture runtime rejects this candidate",
                )]);
            }
            if config.outputs[0].label == "Stale fixture" {
                config.revision = Uuid::new_v4();
            }
            let clock = Arc::new(MonotonicClock::default());
            let registry = SourceRegistry::build(&config, clock.clone(), |source| {
                let index = config
                    .sources
                    .iter()
                    .position(|s| s.id == source.id)
                    .unwrap();
                Ok(Box::new(Mock {
                    device: devices[index].clone(),
                    values: Values::from([
                        ("issafe".into(), json!(true)),
                        ("channel/0".into(), json!(1)),
                        ("temperature".into(), json!(20)),
                    ]),
                }))
            })?;
            HubRuntime::from_registry(config, Arc::new(registry), clock)
        }),
    )
    .unwrap()
}

#[tokio::test]
async fn configuration_transfer_is_revision_owned_inert_and_uses_ordinary_apply() {
    use regain_hub::transfer::ImportMode;
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let service = managed(&f, Some(path.clone()));
    let before = service.configuration();
    let mut document = service.export_configuration(before.revision).unwrap();
    document.configuration.outputs[0].label = "Imported switch label".into();
    let text = serde_json::to_string(&document).unwrap();
    assert!(matches!(
        service.export_configuration(Uuid::new_v4()),
        Err(UpdateError::Conflict)
    ));
    assert!(matches!(
        service.prepare_import(Uuid::new_v4(), &text, ImportMode::Restore),
        Err(UpdateError::Conflict)
    ));
    let prepared = service
        .prepare_import(before.revision, &text, ImportMode::Restore)
        .unwrap();
    assert_eq!(service.configuration(), before);
    assert!(!path.exists());
    assert_eq!(service.runtime().unwrap().active_connections(), 0);
    assert!(
        f.devices
            .iter()
            .all(|d| d.connects.load(SeqCst) == 0 && d.disconnects.load(SeqCst) == 0)
    );
    let applied = service
        .apply(before.revision, prepared.candidate)
        .await
        .unwrap();
    assert!(applied.applied && applied.ready);
    assert_eq!(
        ConfigStore::load(&path).unwrap().snapshot().outputs[0].label,
        "Imported switch label"
    );
    assert!(matches!(
        service.prepare_import(before.revision, &text, ImportMode::Restore),
        Err(UpdateError::Conflict)
    ));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn apply_replaces_file_and_runtime_and_rebinds_client_identity_without_opening_sources() {
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let service = managed(&f, Some(path.clone()));
    let before = service.configuration();
    std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
    let old = service.runtime().unwrap();
    let client = ServiceClient::default();
    let old_client = client.bind(&old).unwrap();
    let mut next = before.clone();
    next.outputs[0].label = "Updated shared switches".into();
    let applied = service.apply(before.revision, next).await.unwrap();
    assert!(applied.applied && applied.ready);
    let current = service.runtime().unwrap();
    assert_ne!(current.runtime_id(), old.runtime_id());
    assert_eq!(service.host_id(), old.runtime_id());
    assert_eq!(client.bind(&current).unwrap().id(), old_client.id());
    assert_eq!(
        old_client.connect(f.switch).await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    let saved = ConfigStore::load(&path).unwrap().snapshot();
    assert_eq!(saved, service.configuration());
    assert_eq!(saved.revision, applied.configuration_revision);
    assert_eq!(saved.identities, before.identities);
    assert!(matches!(
        service.apply(before.revision, before).await,
        Err(UpdateError::Conflict)
    ));
    assert!(
        f.devices
            .iter()
            .all(|d| d.connects.load(SeqCst) == 0 && d.disconnects.load(SeqCst) == 0)
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_runtime_staging_and_commit_failures_preserve_running_configuration() {
    let f = fixture();
    let dir = tempfile::tempdir().unwrap();
    for path in [
        dir.path().join("missing/hub.json"),
        dir.path().to_path_buf(),
    ] {
        let service = managed(&f, Some(path));
        let before = service.configuration();
        let runtime = service.runtime().unwrap();
        assert!(matches!(
            service.apply(before.revision, before.clone()).await,
            Err(UpdateError::Io)
        ));
        assert_eq!(service.configuration(), before);
        assert_eq!(
            service.runtime().unwrap().runtime_id(),
            runtime.runtime_id()
        );
        let client = runtime.client();
        client.connect(f.switch).await.unwrap();
        client.close();
        service.shutdown().await.unwrap();
    }
    let path = dir.path().join("valid.json");
    let service = managed(&f, Some(path.clone()));
    let before = service.configuration();
    std::fs::write(&path, serde_json::to_vec(&before).unwrap()).unwrap();
    for label in ["Unsupported fixture", "Stale fixture"] {
        let mut next = before.clone();
        next.outputs[0].label = label.into();
        assert!(matches!(
            service.apply(before.revision, next).await,
            Err(UpdateError::Invalid(_))
        ));
    }
    assert_eq!(ConfigStore::load(&path).unwrap().snapshot(), before);
    assert_eq!(service.configuration(), before);
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "Staged files must be removed"
    );
    service.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn pending_connections_and_retained_writes_prevent_configuration_apply() {
    let f = fixture();
    let service = managed(&f, None);
    let before = service.configuration();
    let client = service.runtime().unwrap().client();
    f.devices[0].hang_connect.store(true, SeqCst);
    let connecting = tokio::spawn({
        let client = client.clone();
        let id = f.switch;
        async move { client.connect(id).await }
    });
    settle().await;
    assert!(matches!(
        service.apply(before.revision, before.clone()).await,
        Err(UpdateError::Connected)
    ));
    client.close();
    connecting.await.unwrap().unwrap_err();
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    f.devices[0].hang_connect.store(false, SeqCst);
    let client = service.runtime().unwrap().client();
    client.connect(f.switch).await.unwrap();
    assert!(matches!(
        service.apply(before.revision, before.clone()).await,
        Err(UpdateError::Connected)
    ));
    f.devices[0].hang_write.store(true, SeqCst);
    let held = client.connection(f.switch).unwrap();
    let writing = tokio::spawn(async move { held.switch().unwrap().set_value(0, 0.0).await });
    settle().await;
    client.close();
    assert!(matches!(
        service.apply(before.revision, before.clone()).await,
        Err(UpdateError::Connected)
    ));
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(
        writing.await.unwrap().unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(service.configuration(), before);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_apply_finishes_commit_and_blocks_activation_after_uncertain_cleanup() {
    let mut f = fixture();
    f.config.sources[0].polling.request_timeout_seconds = 3.0;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let service = managed(&f, Some(path.clone()));
    let before = service.configuration();
    let previous = service.runtime().unwrap();
    let client = previous.client();
    client.connect(f.switch).await.unwrap();
    f.devices[0].hang_disconnect.store(true, SeqCst);
    client.close();
    let applying = tokio::spawn({
        let service = service.clone();
        let next = before.clone();
        async move { service.apply(next.revision, next).await }
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while service.configuration().revision == before.revision {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        service.apply(before.revision, before.clone()).await,
        Err(UpdateError::Busy)
    ));
    assert!(previous.client().connect(f.switch).await.is_err());
    applying.abort();
    assert!(applying.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        while serde_json::to_value(service.status()).unwrap()["phase"] == "applying" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(service.status()).unwrap()["phase"],
        "blocked"
    );
    assert_eq!(
        service.runtime().err().unwrap().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        ConfigStore::load(&path).unwrap().snapshot(),
        service.configuration()
    );
    assert_ne!(service.configuration().revision, before.revision);
    assert_eq!(f.devices[0].disconnects.load(SeqCst), 1);
    assert_eq!(f.devices[0].connects.load(SeqCst), 1);
    assert_eq!(
        service.shutdown().await.unwrap_err()[0].1.kind,
        ErrorKind::Uncertain
    );
    assert_eq!(f.devices[0].disconnects.load(SeqCst), 1);
}

#[tokio::test]
async fn competing_edits_cannot_both_commit_and_no_old_client_reopens_retired_sources() {
    let f = fixture();
    let service = managed(&f, None);
    let before = service.configuration();
    let old = service.runtime().unwrap();
    let client = old.client();
    let (a, b) = tokio::join!(
        service.apply(before.revision, before.clone()),
        service.apply(before.revision, before.clone())
    );
    assert_eq!(
        [a.is_ok(), b.is_ok()].into_iter().filter(|ok| *ok).count(),
        1
    );
    assert!(matches!(
        a.err().or_else(|| b.err()),
        Some(UpdateError::Busy | UpdateError::Conflict)
    ));
    assert!(client.connect(f.switch).await.is_err());
    assert!(f.devices.iter().all(|d| d.connects.load(SeqCst) == 0));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_panicking_update_blocks_admission_and_can_still_shutdown() {
    let f = fixture();
    let service = managed(&f, None);
    let before = service.configuration();
    let previous = service.runtime().unwrap();
    let mut next = before.clone();
    next.outputs[0].label = "Panicking fixture".into();
    assert!(matches!(
        service.apply(before.revision, next).await,
        Err(UpdateError::Task)
    ));
    assert_eq!(service.configuration(), before);
    assert_eq!(
        serde_json::to_value(service.status()).unwrap()["phase"],
        "blocked"
    );
    assert!(previous.client().connect(f.switch).await.is_err());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn ipc_apply_deadline_reports_uncertainty_and_keeps_committed_status_readable() {
    use regain_hub::ipc::{Limits, read_frame, serve_service_stream};
    use tokio::io::{AsyncWriteExt, DuplexStream};
    async fn rpc(stream: &mut DuplexStream, id: u64, command: Value) -> Value {
        let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(&bytes).await.unwrap();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            read_frame(stream, Duration::from_secs(2)),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        serde_json::from_slice(&response).unwrap()
    }
    let f = fixture();
    let service = managed(&f, None);
    let before = service.configuration();
    let client = service.runtime().unwrap().client();
    client.connect(f.switch).await.unwrap();
    f.devices[0].hang_disconnect.store(true, SeqCst);
    client.close();
    let (mut stream, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(serve_service_stream(
        server,
        service.clone(),
        Limits {
            frame_timeout: Duration::from_secs(2),
            operation_timeout: Duration::from_millis(50),
        },
    ));
    let hello = rpc(&mut stream, 1, json!({"op":"hello"})).await;
    assert!(
        hello["result"]["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("applyConfig"))
    );
    let outcome = rpc(
        &mut stream,
        2,
        json!({"op":"applyConfig", "expectedRevision":before.revision,"candidate":before}),
    )
    .await;
    assert_eq!(outcome["error"]["code"], "uncertain");
    let snapshot = rpc(&mut stream, 3, json!({"op":"getConfig"})).await;
    assert_ne!(snapshot["result"]["revision"], before.revision.to_string());
    tokio::time::timeout(Duration::from_secs(3), async {
        while serde_json::to_value(service.status()).unwrap()["phase"] == "applying" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let status = rpc(&mut stream, 4, json!({"op":"hostStatus"})).await;
    assert_eq!(status["result"]["phase"], "blocked");
    assert_eq!(
        status["result"]["configurationRevision"],
        snapshot["result"]["revision"]
    );
    assert_eq!(f.devices[0].disconnects.load(SeqCst), 1);
    drop(stream);
    task.await.unwrap().unwrap();
    assert_eq!(
        service.shutdown().await.unwrap_err()[0].1.kind,
        ErrorKind::Uncertain
    );
}
