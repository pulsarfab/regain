//! Runtime-owned native camera supervision with explicit production simulations.
use super::*;
use crate::{
    camera::{
        acquisition::ExposureRequest,
        image::{ElementType, ImageDescriptor, ImageOrder},
        native_owner::NativeCamera,
        native_source::NativeCameraBackend,
        properties::CameraSetting,
        runtime::NativeCameraRuntime,
    },
    config::{SourceBackend, SourceConfig},
    factory::NoCredentials,
    safety::MonotonicClock,
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

fn config(direct: bool) -> HubConfig {
    let mut config = HubConfig::empty();
    config.sources.push(
        serde_json::from_value(json!({
            "id":Uuid::new_v4(), "label":"Runtime camera simulation",
            "backend":{
                "kind":"native", "device":if direct {"camera-direct"} else {"camera-sdk"},
                "identity":if direct {"direct-simulator"} else {"sim00001"},
                "camera":{
                    "model":if direct {"ZWO ASI585MM Pro"} else {"ZWO Simulated"},
                    "recovery":{"maxRetries":0,"readyFrameDownloadRetries":0,
                        "reconnectDelaySeconds":0.01}
                }
            },
            "polling":{"connectionTimeoutSeconds":10.,"requestTimeoutSeconds":5.,"pollSeconds":60.}
        }))
        .unwrap(),
    );
    config
}

fn native(resources: CameraResources) -> NativeRuntime {
    NativeRuntime {
        directory: std::env::var_os("REGAIN_TEST_WORKERS")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")),
        simulate: true,
        references: None,
        cameras: Some(NativeCameraRuntime {
            sdk: "unused".into(),
            sdk_simulation: Some(json!({"instant":false})),
            resources,
            diagnostic: Arc::new(|_, _, _| {}),
        }),
    }
}

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn runtime_constructs_distinct_inert_supervisors_before_adopting_host_resources() {
    let resources = CameraResources::default();
    let mut native = native(resources.clone());
    let directory = tempfile::tempdir().unwrap();
    native.directory = directory.path().join("missing-workers");
    native.simulate = false;
    let camera = native.cameras.as_mut().unwrap();
    camera.sdk = directory.path().join("missing-sdk");
    camera.sdk_simulation = None;
    let mut config = config(false);
    config.sources.push(self::config(true).sources.remove(0));
    let runtime = HubRuntime::build(
        config.clone(),
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    tokio::task::yield_now().await;
    assert_eq!(runtime.cameras.len(), 2);
    assert!(runtime.activity.shares(&resources.activity()));
    assert!(!Arc::ptr_eq(
        &runtime.cameras[&config.sources[0].id],
        &runtime.cameras[&config.sources[1].id]
    ));
    for source in &config.sources {
        let state = runtime.source_snapshot(source.id).unwrap();
        assert!(!state.transport_connected);
        assert_eq!(state.lease_count, 0);
        let acquisition = runtime.camera_acquisition_status(source.id).unwrap();
        assert_eq!(acquisition.source, source.id);
        assert!(acquisition.acquisition.is_none() && !acquisition.image_ready);
    }
    assert_eq!(resources.activity().active(), 0);
    assert_eq!(resources.image_budget().used_bytes(), 0);
    assert!(runtime.outputs().is_empty());
    assert!(
        !runtime
            .configuration_capabilities()
            .contains(&"cameraOutputs")
    );
    assert_eq!(
        runtime
            .camera_acquisition_status(Uuid::new_v4())
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn injected_native_registry_adopts_accounting_and_rejects_mixed_host_resources() {
    let resources = CameraResources::new(512 * 1024).unwrap();
    let cfg = config(false);
    let clock = Arc::new(MonotonicClock::default());
    let registry = build_sources_bound(
        &cfg,
        &native(resources.clone()),
        &NoCredentials,
        clock.clone(),
        None,
    )
    .unwrap();
    let runtime = HubRuntime::from_registry(cfg.clone(), registry.clone(), clock.clone()).unwrap();
    assert!(runtime.activity.shares(&resources.activity()));
    // Both identities matter: matching only bytes or only the counter is unsafe.
    for mismatched in [
        CameraResources::from_parts(resources.image_budget(), ActivityCounter::default()),
        CameraResources::from_parts(
            crate::camera::image::ImageBudget::new(512 * 1024).unwrap(),
            resources.activity(),
        ),
    ] {
        assert_eq!(
            CameraSupervisor::new(
                registry.get(cfg.sources[0].id).unwrap(),
                mismatched.image_budget(),
                AcquisitionTiming::default(),
                mismatched.activity(),
            )
            .err()
            .unwrap()
            .kind,
            ErrorKind::InvalidValue,
        );
        let errors = HubRuntime::from_registry_with_camera_resources(
            cfg.clone(),
            registry.clone(),
            clock.clone(),
            mismatched,
        )
        .err()
        .unwrap();
        assert_eq!(errors[0].path, "sources[0].backend");
        assert_eq!(errors[0].code, "cameraResources");
    }
    let mut mixed = cfg;
    let mut other: SourceConfig = mixed.sources[0].clone();
    other.id = Uuid::new_v4();
    if let SourceBackend::Native { identity, .. } = &mut other.backend {
        *identity = "sim00002".into();
    }
    mixed.sources.push(other);
    let registry = Arc::new(
        SourceRegistry::build(&mixed, clock.clone(), |source| {
            let selected = if source.id == mixed.sources[0].id {
                resources.clone()
            } else {
                CameraResources::default()
            };
            let owner = NativeCamera::new(
                regain_core::Selection {
                    name: "ZWO Simulated".into(),
                    serial: None,
                    direct: false,
                    sdk_fallback: false,
                    recovery: Default::default(),
                },
                regain_core::Runtime {
                    directory: "missing-workers".into(),
                    sdk: "missing-sdk".into(),
                    simulate: true,
                    sdk_simulation: Some(json!({"instant":true})),
                },
                selected.image_budget(),
                selected.activity(),
                Arc::new(|_, _, _| {}),
            )?;
            Ok(Box::new(NativeCameraBackend::new(owner, vec![])?))
        })
        .unwrap(),
    );
    let errors = HubRuntime::from_registry(mixed, registry.clone(), clock)
        .err()
        .unwrap();
    assert_eq!(errors[0].path, "sources[1].backend");
    assert_eq!(errors[0].code, "cameraResources");
    registry.shutdown().await.unwrap();
    runtime.shutdown().await.unwrap();
    assert_eq!(resources.activity().active(), 0);
    assert_eq!(resources.image_budget().used_bytes(), 0);
}

#[tokio::test]
async fn runtime_camera_ownership_and_image_budget_survive_client_loss_and_revision_retirement() {
    for direct in [false, true] {
        const CAPACITY: usize = 512 * 1024;
        let resources = CameraResources::new(CAPACITY).unwrap();
        let budget = resources.image_budget();
        let native = native(resources.clone());
        let cfg = config(direct);
        let source_id = cfg.sources[0].id;
        let runtime = HubRuntime::build(
            cfg.clone(),
            &native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let camera = runtime.cameras[&source_id].clone();
        let first = camera.connect().await.unwrap();
        let sibling = runtime.cameras[&source_id].connect().await.unwrap();
        first.set(CameraSetting::NumX(64)).await.unwrap();
        first.set(CameraSetting::NumY(64)).await.unwrap();
        let request = ExposureRequest {
            duration_seconds: 0.3,
            light: false,
        };
        let acquisition = first.start(request).await.unwrap();
        assert_eq!(
            runtime
                .camera_acquisition_status(source_id)
                .unwrap()
                .acquisition,
            Some(acquisition)
        );
        assert_eq!(
            sibling.start(request).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        drop(first);
        assert_eq!(runtime.quiesce().err().unwrap().kind, ErrorKind::Busy);
        until(|| {
            runtime
                .camera_acquisition_status(source_id)
                .unwrap()
                .image_ready
        })
        .await;
        until(|| resources.activity().active() == 0).await;
        let pinned = sibling.image().unwrap();
        let original = pinned.image.bytes().to_vec();
        let old_charge = budget.used_bytes();
        assert!(old_charge > 0);
        drop(sibling);
        until(|| resources.activity().active() == 0).await;
        drop(runtime.quiesce().unwrap());
        runtime.shutdown().await.unwrap();
        assert!(
            !runtime
                .camera_acquisition_status(source_id)
                .unwrap()
                .image_ready
        );
        assert_eq!(
            camera.connect().await.err().unwrap().kind,
            ErrorKind::Disconnected
        );
        assert_eq!(budget.used_bytes(), old_charge);
        drop(camera);
        drop(runtime);
        assert_eq!(budget.used_bytes(), old_charge);
        let mut next = cfg;
        next.revision = Uuid::new_v4();
        let runtime = HubRuntime::build(
            next,
            &native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let second = runtime.cameras[&source_id].connect().await.unwrap();
        second.set(CameraSetting::NumX(64)).await.unwrap();
        second.set(CameraSetting::NumY(64)).await.unwrap();
        let fill = budget
            .allocate(
                ImageDescriptor::new(
                    (CAPACITY - old_charge) as u32,
                    1,
                    None,
                    ElementType::Byte,
                    ElementType::Byte,
                    ImageOrder::SensorRows,
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            second.start(request).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        drop(fill);
        second.start(request).await.unwrap();
        until(|| second.status().image_ready).await;
        assert_eq!(pinned.image.bytes(), original);
        assert_ne!(second.image().unwrap().identity.acquisition, acquisition);
        drop(second);
        runtime.shutdown().await.unwrap();
        until(|| resources.activity().active() == 0).await;
        // A retained closed runtime must not pin its own completed image.
        assert_eq!(budget.used_bytes(), old_charge);
        drop(runtime);
        until(|| resources.activity().active() == 0).await;
        assert_eq!(budget.used_bytes(), old_charge);
        drop(pinned);
        assert_eq!(budget.used_bytes(), 0);
    }
}

#[tokio::test]
async fn proxy_only_hosts_share_resources_without_sdk_settings_or_network_io() {
    let resources = CameraResources::default();
    let mut cfg = HubConfig::empty();
    cfg.sources.push(
        serde_json::from_value(json!({
            "id":Uuid::new_v4(), "label":"Inert remote camera",
            "backend":{"kind":"alpaca","baseUrl":"http://127.0.0.1:1",
                "deviceType":"camera","deviceNumber":7}
        }))
        .unwrap(),
    );
    let native = NativeRuntime {
        directory: "missing-workers".into(),
        simulate: false,
        references: None,
        cameras: None,
    };
    let first = HubRuntime::build_with_camera_resources(
        cfg.clone(),
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
        resources.clone(),
    )
    .unwrap();
    cfg.revision = Uuid::new_v4();
    let second = HubRuntime::build_with_camera_resources(
        cfg,
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
        resources.clone(),
    )
    .unwrap();
    let work = Activity::new(resources.activity());
    assert_eq!(first.quiesce().err().unwrap().kind, ErrorKind::Busy);
    assert_eq!(second.quiesce().err().unwrap().kind, ErrorKind::Busy);
    drop(work);
    assert_eq!(resources.image_budget().used_bytes(), 0);
    for runtime in [first, second] {
        assert_eq!(runtime.cameras.len(), 1);
        assert!(
            runtime
                .source_snapshots()
                .iter()
                .all(|source| !source.transport_connected && source.lease_count == 0)
        );
        drop(runtime.quiesce().unwrap());
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn runtime_retirement_preserves_live_uncertainty_then_releases_only_after_source_drain() {
    let resources = CameraResources::new(512 * 1024).unwrap();
    let mut native = native(resources.clone());
    native.cameras.as_mut().unwrap().sdk_simulation =
        Some(json!({"instant":true,"fault":"download"}));
    let cfg = config(false);
    let source_id = cfg.sources[0].id;
    let runtime = HubRuntime::build(
        cfg,
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let camera = runtime.cameras[&source_id].clone();
    let client = camera.connect().await.unwrap();
    client.set(CameraSetting::NumX(64)).await.unwrap();
    client.set(CameraSetting::NumY(64)).await.unwrap();
    let acquisition = client
        .start(ExposureRequest {
            duration_seconds: 0.01,
            light: false,
        })
        .await
        .unwrap();
    until(|| camera.status().phase == crate::camera::acquisition::AcquisitionPhase::Uncertain)
        .await;
    camera.retire_after_source_shutdown();
    assert_eq!(camera.status().acquisition, Some(acquisition));
    assert_eq!(runtime.quiesce().err().unwrap().kind, ErrorKind::Busy);
    let error = camera.status().error.unwrap();
    runtime.shutdown().await.unwrap();
    until(|| resources.activity().active() == 0).await;
    assert!(camera.status().acquisition.is_none());
    assert_eq!(camera.status().error.unwrap().kind, error.kind);
    assert_eq!(
        camera.connect().await.err().unwrap().kind,
        ErrorKind::Disconnected
    );
    assert_eq!(
        client
            .start(ExposureRequest {
                duration_seconds: 0.01,
                light: false
            })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Disconnected
    );
    assert_eq!(resources.image_budget().used_bytes(), 0);
    runtime.shutdown().await.unwrap();
}

fn outputs(cfg: &mut HubConfig, source: Uuid, numbers: &[u32]) -> Vec<Uuid> {
    numbers
        .iter()
        .map(|number| {
            let id = Uuid::new_v4();
            cfg.outputs.push(crate::config::OutputConfig {
                id,
                number: *number,
                label: format!("Camera output {number}"),
                device: VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::Camera,
                },
            });
            id
        })
        .collect()
}

#[tokio::test]
async fn camera_output_leases_share_acquisition_and_survive_owner_disconnect() {
    use crate::ipc::{Get, Put};
    for direct in [false, true] {
        let resources = CameraResources::new(1024 * 1024).unwrap();
        let mut cfg = config(direct);
        let source = cfg.sources[0].id;
        let ids = outputs(&mut cfg, source, &[2, 9]);
        let runtime = HubRuntime::build(
            cfg,
            &native(resources.clone()),
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let first = runtime.client();
        let sibling = runtime.client();
        first.connect(ids[0]).await.unwrap();
        sibling.connect(ids[1]).await.unwrap();
        let owner = first.connection(ids[0]).unwrap();
        let observer = sibling.connection(ids[1]).unwrap();
        assert!(owner.connected() && observer.connected());
        assert_eq!(runtime.source_snapshot(source).unwrap().lease_count, 2);
        for setting in [
            CameraSetting::NumX(64),
            CameraSetting::NumY(64),
            CameraSetting::Gain(123),
        ] {
            owner.put(Put::CameraSetting { setting }).await.unwrap();
        }
        assert_eq!(
            observer
                .get(Get::Camera {
                    property: crate::camera::properties::CameraProperty::Gain
                })
                .await
                .unwrap(),
            json!(123)
        );
        let request = ExposureRequest {
            duration_seconds: 0.3,
            light: false,
        };
        let acquisition = owner.put(Put::StartExposure { request }).await.unwrap();
        assert_eq!(
            observer
                .put(Put::StartExposure { request })
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Busy
        );
        assert_eq!(
            observer.put(Put::AbortExposure {}).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(
            observer
                .put(Put::CameraSetting {
                    setting: CameraSetting::Gain(124)
                })
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Busy
        );
        first.disconnect_checked(ids[0]).unwrap();
        drop(owner);
        until(|| observer.camera().unwrap().status().image_ready).await;
        assert_eq!(
            observer.get(Get::CameraAcquisition {}).await.unwrap()["completed"]["acquisition"],
            acquisition
        );
        let pinned = observer.camera().unwrap().image().unwrap();
        let original = pinned.image.bytes().to_vec();
        let cached = runtime.output_status(ids[1], 0, 1).unwrap();
        let encoded = serde_json::to_value(cached).unwrap();
        assert_eq!(
            encoded["diagnostics"]["acquisition"]["completed"]["acquisition"],
            acquisition
        );
        assert_eq!(
            encoded["diagnostics"]["health"]["generation"],
            encoded["diagnostics"]["acquisition"]["generation"]
        );
        assert!(!encoded.to_string().contains("pixels"));
        assert!(runtime.output_status(ids[1], 2, 1).is_err());
        assert!(serde_json::to_value(runtime.output_status(ids[1],1,1).unwrap()).unwrap()["diagnostics"]["acquisition"].is_null());
        let state = observer.get(Get::DeviceState {}).await.unwrap();
        assert!(
            state
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value["Name"] == "ImageReady" && value["Value"] == true)
        );
        assert!(
            !state
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value["Name"] == "TimeStamp")
        );
        assert_eq!(runtime.quiesce().err().unwrap().kind, ErrorKind::Busy);
        sibling.disconnect_checked(ids[1]).unwrap();
        drop(observer);
        until(|| runtime.active_connections() == 0).await;
        drop(runtime.quiesce().unwrap());
        runtime.shutdown().await.unwrap();
        assert_eq!(pinned.image.bytes(), original);
        drop(pinned);
        assert_eq!(resources.image_budget().used_bytes(), 0);
    }
}

#[tokio::test]
async fn independent_camera_sources_can_acquire_concurrently_through_one_runtime_client() {
    let resources = CameraResources::new(2 * 1024 * 1024).unwrap();
    let mut cfg = config(false);
    let sdk = cfg.sources[0].id;
    let direct = config(true).sources.remove(0);
    let direct_id = direct.id;
    cfg.sources.push(direct);
    let first_id = outputs(&mut cfg, sdk, &[2])[0];
    let second_id = outputs(&mut cfg, direct_id, &[9])[0];
    let runtime = HubRuntime::build(
        cfg,
        &native(resources.clone()),
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let client = runtime.client();
    client.connect(first_id).await.unwrap();
    client.connect(second_id).await.unwrap();
    let first = client.connection(first_id).unwrap();
    let second = client.connection(second_id).unwrap();
    for connection in [&first, &second] {
        connection
            .camera()
            .unwrap()
            .set(CameraSetting::NumX(64))
            .await
            .unwrap();
        connection
            .camera()
            .unwrap()
            .set(CameraSetting::NumY(64))
            .await
            .unwrap();
    }
    let request = ExposureRequest {
        duration_seconds: 0.3,
        light: false,
    };
    let a = first.camera().unwrap().start(request).await.unwrap();
    let b = second.camera().unwrap().start(request).await.unwrap();
    assert_ne!(a, b);
    until(|| {
        first.camera().unwrap().status().image_ready
            && second.camera().unwrap().status().image_ready
    })
    .await;
    assert_eq!(
        first.camera().unwrap().image().unwrap().identity.source,
        sdk
    );
    assert_eq!(
        second.camera().unwrap().image().unwrap().identity.source,
        direct_id
    );
    runtime.shutdown().await.unwrap();
    assert!(!first.connected() && !second.connected());
    drop(first);
    drop(second);
    until(|| runtime.active_connections() == 0).await;
    assert_eq!(resources.image_budget().used_bytes(), 0);
}

#[tokio::test]
async fn camera_acquisition_ipc_retains_one_owner_after_stream_loss_and_uses_scalar_metadata_only()
{
    use crate::{
        client::{Client, ClientLimits},
        ipc::{Command, Get, Limits, Put, serve_stream},
    };
    let resources = CameraResources::new(1024 * 1024).unwrap();
    let mut cfg = config(false);
    let source = cfg.sources[0].id;
    let ids = outputs(&mut cfg, source, &[2, 9]);
    let runtime = HubRuntime::build(
        cfg,
        &native(resources.clone()),
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let (first_stream, first_server) = tokio::io::duplex(8192);
    let first_task = tokio::spawn(serve_stream(
        first_server,
        runtime.clone(),
        Limits::default(),
    ));
    let first = Client::from_stream(
        first_stream,
        runtime.instance_id(),
        Duration::from_secs(3),
        ClientLimits::default(),
    )
    .await
    .unwrap();
    let (second_stream, second_server) = tokio::io::duplex(8192);
    let second_task = tokio::spawn(serve_stream(
        second_server,
        runtime.clone(),
        Limits::default(),
    ));
    let second = Client::from_stream(
        second_stream,
        runtime.instance_id(),
        Duration::from_secs(3),
        ClientLimits::default(),
    )
    .await
    .unwrap();
    assert!(
        first
            .hello()
            .capabilities
            .iter()
            .any(|value| value == "cameraAcquisition")
    );
    assert!(
        !first
            .hello()
            .capabilities
            .iter()
            .any(|value| value == "cameraOutputs")
    );
    first
        .request(Command::Connect { output: ids[0] })
        .await
        .unwrap();
    second
        .request(Command::Connect { output: ids[1] })
        .await
        .unwrap();
    for setting in [CameraSetting::NumX(64), CameraSetting::NumY(64)] {
        first
            .request(Command::Put {
                output: ids[0],
                property: Put::CameraSetting { setting },
            })
            .await
            .unwrap();
    }
    let acquisition = first
        .request(Command::Put {
            output: ids[0],
            property: Put::StartExposure {
                request: ExposureRequest {
                    duration_seconds: 0.3,
                    light: false,
                },
            },
        })
        .await
        .unwrap();
    first.close();
    until(|| {
        runtime
            .camera_acquisition_status(source)
            .unwrap()
            .image_ready
    })
    .await;
    let status = second
        .request(Command::Get {
            output: ids[1],
            property: Get::CameraAcquisition {},
        })
        .await
        .unwrap();
    assert_eq!(status["completed"]["acquisition"], acquisition);
    assert!(!status.to_string().contains("pixels"));
    let gain = second
        .request(Command::Get {
            output: ids[1],
            property: Get::Camera {
                property: crate::camera::properties::CameraProperty::Gain,
            },
        })
        .await
        .unwrap();
    assert!(gain.as_i64().is_some());
    second
        .request(Command::Disconnect { output: ids[1] })
        .await
        .unwrap();
    second.close();
    first_task.await.unwrap().unwrap();
    second_task.await.unwrap().unwrap();
    until(|| runtime.active_connections() == 0).await;
    runtime.shutdown().await.unwrap();
    assert_eq!(resources.image_budget().used_bytes(), 0);
}
