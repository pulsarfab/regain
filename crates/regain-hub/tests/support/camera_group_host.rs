use super::*;
use regain_hub::{
    config::{ConfigStore, HubConfig},
    coordination::{CameraMemberRequest, HostedCameraPhase as Phase, HostedCameraStatus},
    runtime::HubRuntime,
    service::HubService,
};
use tokio::io::{AsyncWriteExt, DuplexStream};

fn config() -> HubConfig {
    serde_json::from_str(include_str!("../../examples/paired-cameras.json")).unwrap()
}
fn devices() -> Arc<Vec<Arc<Device>>> {
    Arc::new((0..3).map(|_| Arc::new(Device::default())).collect())
}
fn runtime(config: HubConfig, devices: &Arc<Vec<Arc<Device>>>) -> Arc<HubRuntime> {
    let clock = Arc::new(MonotonicClock::default());
    let registry = regain_hub::source::SourceRegistry::build(&config, clock.clone(), |source| {
        let index = config
            .sources
            .iter()
            .position(|s| s.id == source.id)
            .unwrap();
        Ok(Box::new(Mock(devices[index].clone())))
    })
    .unwrap();
    HubRuntime::from_registry(config, Arc::new(registry), clock).unwrap()
}
fn requests(config: &HubConfig) -> Vec<CameraMemberRequest> {
    config.camera_groups[0]
        .members
        .iter()
        .map(|source| CameraMemberRequest {
            source: *source,
            exposure: request(),
            require_scalar_image: false,
        })
        .collect()
}
async fn wait_starts(devices: &[Arc<Device>], count: usize) {
    for _ in 0..100 {
        settle().await;
        if devices
            .iter()
            .take(2)
            .all(|d| d.starts.load(SeqCst) == count)
        {
            return;
        }
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    panic!("Expected {count} starts");
}
async fn terminal(runtime: &HubRuntime, revision: Uuid, group: Uuid) -> HostedCameraStatus {
    for _ in 0..1000 {
        settle().await;
        let status = runtime.camera_group_status(revision, group, None).unwrap();
        if !matches!(status.phase, Phase::Connecting | Phase::Running) {
            return status;
        }
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    panic!("Hosted camera group did not finish");
}
async fn idle(runtime: &HubRuntime) {
    for _ in 0..1000 {
        settle().await;
        if runtime.active_connections() == 0
            && runtime
                .source_snapshots()
                .iter()
                .all(|s| s.lease_count == 0)
        {
            return;
        }
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    panic!("Runtime retained work: {}", runtime.active_connections());
}
async fn send(stream: &mut DuplexStream, id: u64, command: Value) {
    let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .unwrap();
    stream.write_all(&bytes).await.unwrap();
}
async fn call(stream: &mut DuplexStream, id: u64, command: Value) -> Value {
    send(stream, id, command).await;
    serde_json::from_slice(
        &regain_hub::ipc::read_frame(stream, Duration::from_secs(2))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
async fn stream(
    service: Arc<HubService>,
) -> (
    DuplexStream,
    tokio::task::JoinHandle<Result<(), regain_hub::ipc::ProtocolError>>,
) {
    let (mut reader, server) = tokio::io::duplex(1024 * 1024);
    let task = tokio::spawn(regain_hub::ipc::serve_service_stream(
        server,
        service,
        Default::default(),
    ));
    let hello = call(&mut reader, 1, json!({"op":"hello"})).await;
    assert!(
        hello["result"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("cameraGroups"))
    );
    assert!(
        hello["result"]["operations"]
            .as_array()
            .unwrap()
            .contains(&json!("cameraGroupImage"))
    );
    (reader, task)
}
#[tokio::test(start_paused = true)]
async fn saved_camera_group_survives_read_or_unread_ack_eof_and_exports_exact_retained_images() {
    use regain_hub::camera::ipc_image::{GroupImageRequest, download_group_from_stream};
    for read_ack in [false, true] {
        let config = config();
        let revision = config.revision;
        let group = config.camera_groups[0].id;
        let devices = devices();
        let runtime = runtime(config.clone(), &devices);
        let service = HubService::read_only(runtime.clone());
        let (mut first, server) = stream(service.clone()).await;
        let query = |operation: Option<Uuid>| json!({"op":"cameraGroupStatus","group":group,"operation":operation,"expectedRevision":revision});
        assert_eq!(
            call(&mut first, 2, query(None)).await["error"]["code"],
            "unavailable"
        );
        assert!(devices.iter().all(|d| d.connects.load(SeqCst) == 0));
        let start = json!({"op":"startCameraGroup","group":group,"requests":requests(&config),"expectedRevision":revision});
        let ack = if read_ack {
            Some(call(&mut first, 3, start.clone()).await["result"].clone())
        } else {
            send(&mut first, 3, start).await;
            None
        };
        wait_starts(&devices, 1).await;
        assert!(runtime.active_connections() > 0);
        drop(first);
        let _ = server.await.unwrap();
        let (mut second, server) = stream(service.clone()).await;
        let recovered = call(&mut second, 2, query(None)).await["result"].clone();
        let operation: Uuid = serde_json::from_value(recovered["operation"].clone()).unwrap();
        if let Some(ack) = ack {
            assert_eq!(ack["operation"], recovered["operation"]);
        }
        assert_eq!(
            recovered["bindings"][0]["configuredSource"],
            json!(config.sources[2].id)
        );
        assert_eq!(
            recovered["bindings"][0]["physicalSource"],
            json!(config.sources[0].id)
        );
        assert_eq!(devices[2].connects.load(SeqCst), 0);
        for d in devices.iter().take(2) {
            d.complete();
        }
        let complete = terminal(&runtime, revision, group).await;
        assert_eq!(complete.phase, Phase::Complete);
        idle(&runtime).await;
        let result = complete.result.unwrap();
        for member in &result.members {
            assert_ne!(
                runtime
                    .source_snapshots()
                    .iter()
                    .find(|s| s.source == member.source)
                    .unwrap()
                    .generation,
                member.generation,
                "Completed group image must remain readable after transport retirement"
            );
            let request = GroupImageRequest {
                host_instance: service.host_id(),
                configuration_revision: revision,
                group,
                operation,
                source: member.source,
                generation: member.generation,
                acquisition: member.acquisition.unwrap(),
            };
            let (reader, server) = tokio::io::duplex(1024 * 1024);
            let task = tokio::spawn(regain_hub::ipc::serve_service_stream(
                server,
                service.clone(),
                Default::default(),
            ));
            let downloaded = download_group_from_stream(
                reader,
                service.instance_id(),
                request,
                &ImageBudget::new(24).unwrap(),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
            assert_eq!(downloaded.manifest.request, request);
            assert_eq!(downloaded.image.bytes(), &[17; 12]);
            task.await.unwrap().unwrap();
        }
        assert_eq!(devices[0].downloads.load(SeqCst), 1);
        assert_eq!(devices[1].downloads.load(SeqCst), 1);
        assert_eq!(
            call(&mut second, 3, query(Some(Uuid::new_v4()))).await["error"]["code"],
            "unavailable"
        );
        drop(second);
        let _ = server.await.unwrap();
        runtime.shutdown().await.unwrap();
    }
}
#[tokio::test(start_paused = true)]
async fn saved_camera_group_new_start_retires_old_operation_but_not_existing_image_readers() {
    let config = config();
    let revision = config.revision;
    let group = config.camera_groups[0].id;
    let devices = devices();
    let runtime = runtime(config.clone(), &devices);
    let first = runtime
        .start_camera_group(runtime.runtime_id(), revision, group, requests(&config))
        .unwrap();
    wait_starts(&devices, 1).await;
    for d in devices.iter().take(2) {
        d.complete();
    }
    let result = terminal(&runtime, revision, group).await.result.unwrap();
    idle(&runtime).await;
    let image = result.members[0].image.as_ref().unwrap();
    let pinned = runtime
        .camera_group_image(
            revision,
            group,
            first.operation,
            image.source,
            image.generation,
            image.acquisition,
        )
        .unwrap();
    assert!(
        runtime
            .camera_group_image(
                revision,
                group,
                first.operation,
                image.source,
                Uuid::new_v4(),
                image.acquisition
            )
            .is_err()
    );
    let next = runtime
        .start_camera_group(runtime.runtime_id(), revision, group, requests(&config))
        .unwrap();
    assert_ne!(next.operation, first.operation);
    assert!(
        runtime
            .camera_group_status(revision, group, Some(first.operation))
            .is_err()
    );
    assert!(
        runtime
            .camera_group_image(
                revision,
                group,
                first.operation,
                image.source,
                image.generation,
                image.acquisition
            )
            .is_err()
    );
    assert_eq!(pinned.identity.acquisition, image.acquisition);
    wait_starts(&devices, 2).await;
    for d in devices.iter().take(2) {
        d.complete();
    }
    assert_eq!(
        terminal(&runtime, revision, group).await.phase,
        Phase::Complete
    );
    idle(&runtime).await;
    assert_eq!(pinned.image.bytes(), &[17; 12]);
    runtime.shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn saved_camera_group_connection_admission_blocks_apply_and_status_is_inert() {
    let config = config();
    let revision = config.revision;
    let group = config.camera_groups[0].id;
    let devices = devices();
    devices[0].hold_connect.store(true, SeqCst);
    let factory_devices = devices.clone();
    let builder: Arc<regain_hub::service::RuntimeBuilder> =
        Arc::new(move |config| Ok(runtime(config, &factory_devices)));
    let service =
        HubService::persistent(ConfigStore::new(None, config.clone()).unwrap(), builder).unwrap();
    let runtime = service.runtime().unwrap();
    assert!(runtime.camera_group_status(revision, group, None).is_err());
    assert_eq!(devices[0].connects.load(SeqCst), 0);
    let accepted = runtime
        .start_camera_group(service.host_id(), revision, group, requests(&config))
        .unwrap();
    assert!(runtime.active_connections() > 0);
    assert!(matches!(
        service.apply(revision, service.configuration()).await,
        Err(regain_hub::service::UpdateError::Connected)
    ));
    assert!(
        runtime
            .cancel_camera_group(revision, group, Uuid::new_v4())
            .is_err()
    );
    runtime
        .cancel_camera_group(revision, group, accepted.operation)
        .unwrap();
    devices[0].release_connect.notify_one();
    assert_eq!(
        terminal(&runtime, revision, group).await.phase,
        Phase::Cancelled
    );
    idle(&runtime).await;
    let applied = service
        .apply(revision, service.configuration())
        .await
        .unwrap();
    assert!(applied.ready);
    assert!(
        runtime
            .start_camera_group(service.host_id(), revision, group, requests(&config))
            .is_err()
    );
    assert!(
        service
            .runtime()
            .unwrap()
            .camera_group_status(applied.configuration_revision, group, None)
            .is_err()
    );
    service.runtime().unwrap().shutdown().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn saved_camera_group_shutdown_waits_start_ack_and_uses_explicit_abort_policy() {
    let config = config();
    let revision = config.revision;
    let group = config.camera_groups[0].id;
    let devices = devices();
    devices[0].hold_start.store(true, SeqCst);
    let runtime = runtime(config.clone(), &devices);
    runtime
        .start_camera_group(runtime.runtime_id(), revision, group, requests(&config))
        .unwrap();
    wait_starts(&devices, 1).await;
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown().await }
    });
    settle().await;
    assert!(!shutdown.is_finished());
    assert!(runtime.active_connections() > 0);
    assert_eq!(devices[0].aborts.load(SeqCst), 0);
    devices[0].release_start.notify_one();
    shutdown.await.unwrap().unwrap();
    let result = runtime.camera_group_status(revision, group, None).unwrap();
    assert_eq!(result.phase, Phase::Cancelled);
    assert_eq!(devices[0].aborts.load(SeqCst), 1);
    assert_eq!(devices[1].aborts.load(SeqCst), 1);
    assert_eq!(runtime.active_connections(), 0);
    assert_eq!(devices[0].starts.load(SeqCst), 1);
}
#[tokio::test(start_paused = true)]
async fn saved_camera_group_rejects_stale_revision_and_identifies_failed_connection() {
    let mut config = config();
    config.sources[1].polling.connection_timeout_seconds = 1.0;
    let revision = config.revision;
    let group = config.camera_groups[0].id;
    let devices = devices();
    devices[1].hold_connect.store(true, SeqCst);
    let runtime = runtime(config.clone(), &devices);
    assert!(
        runtime
            .start_camera_group(
                runtime.runtime_id(),
                Uuid::new_v4(),
                group,
                requests(&config)
            )
            .is_err()
    );
    assert_eq!(devices[0].connects.load(SeqCst), 0);
    runtime
        .start_camera_group(runtime.runtime_id(), revision, group, requests(&config))
        .unwrap();
    let result = terminal(&runtime, revision, group).await;
    assert_eq!(result.phase, Phase::Failed);
    assert_eq!(result.failed_source, Some(config.sources[1].id));
    assert_eq!(
        devices[0].starts.load(SeqCst) + devices[1].starts.load(SeqCst),
        0
    );
    devices[1].release_connect.notify_one();
    idle(&runtime).await;
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn saved_camera_group_image_rejects_every_identity_mismatch_without_another_download() {
    use regain_hub::camera::ipc_image::{GroupImageRequest, download_group_from_stream};
    let config = config();
    let revision = config.revision;
    let group = config.camera_groups[0].id;
    let devices = devices();
    let runtime = runtime(config.clone(), &devices);
    let service = HubService::read_only(runtime.clone());
    let accepted = runtime
        .start_camera_group(service.host_id(), revision, group, requests(&config))
        .unwrap();
    wait_starts(&devices, 1).await;
    for d in devices.iter().take(2) {
        d.complete();
    }
    let result = terminal(&runtime, revision, group).await.result.unwrap();
    idle(&runtime).await;
    let image = result.members[0].image.as_ref().unwrap();
    let request = GroupImageRequest {
        host_instance: service.host_id(),
        configuration_revision: revision,
        group,
        operation: accepted.operation,
        source: image.source,
        generation: image.generation,
        acquisition: image.acquisition,
    };
    for fault in 0..7 {
        let mut wrong = request;
        let id = Uuid::new_v4();
        match fault {
            0 => wrong.host_instance = id,
            1 => wrong.configuration_revision = id,
            2 => wrong.group = id,
            3 => wrong.operation = id,
            4 => wrong.source = id,
            5 => wrong.generation = id,
            6 => wrong.acquisition = id,
            _ => unreachable!(),
        }
        let (reader, server) = tokio::io::duplex(1024 * 1024);
        let task = tokio::spawn(regain_hub::ipc::serve_service_stream(
            server,
            service.clone(),
            Default::default(),
        ));
        assert!(
            download_group_from_stream(
                reader,
                service.instance_id(),
                wrong,
                &ImageBudget::new(24).unwrap(),
                Duration::from_secs(2)
            )
            .await
            .is_err(),
            "identity {fault}"
        );
        let _ = task.await.unwrap();
    }
    assert_eq!(devices[0].downloads.load(SeqCst), 1);
    assert_eq!(devices[1].downloads.load(SeqCst), 1);
    assert_eq!(
        devices[0].starts.load(SeqCst) + devices[1].starts.load(SeqCst),
        2
    );
    assert_eq!(
        devices[0].aborts.load(SeqCst) + devices[1].aborts.load(SeqCst),
        0
    );
    runtime.shutdown().await.unwrap();
}
#[test]
fn saved_camera_group_commands_require_revision_exact_ids_and_no_unknown_fields() {
    use regain_hub::ipc::Command;
    let group = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let operation = Uuid::new_v4();
    for command in [
        json!({"op":"startCameraGroup","group":group,"requests":[]}),
        json!({"op":"startCameraGroup","group":group,"requests":[],"expectedRevision":revision,"retry":true}),
        json!({"op":"cancelCameraGroup","group":group,"expectedRevision":revision}),
        json!({"op":"cameraGroupStatus","group":group,"operation":operation,"expectedRevision":revision,"connect":true}),
        json!({"op":"startCameraGroup","group":group,"requests":[{"source":group,"exposure":{"durationSeconds":1,"light":true},"retry":true}],"expectedRevision":revision}),
    ] {
        assert!(serde_json::from_value::<Command>(command).is_err());
    }
}
