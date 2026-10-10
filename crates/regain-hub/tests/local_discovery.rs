//! Explicit production simulations only. No USB, serial port, COM activation or LAN search.
use regain_hub::{
    camera::runtime::NativeCameraRuntime,
    config::{HubConfig, NativeDevice, SourceBackend},
    discovery::{Target, discover},
    factory::NoCredentials,
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    service::HubService,
    source::ErrorKind,
};
use std::{path::PathBuf, sync::Arc};
use uuid::Uuid;

fn native() -> Option<NativeRuntime> {
    let directory = PathBuf::from(std::env::var_os("REGAIN_TEST_WORKERS")?);
    for name in ["regain-device", "regain-camera", "regain-alpaca"] {
        assert!(
            directory
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
                .is_file()
        );
    }
    Some(NativeRuntime {
        directory,
        simulate: true,
        references: None,
        cameras: Some(NativeCameraRuntime {
            sdk: PathBuf::from("unused-explicit-simulation-sdk"),
            sdk_simulation: None,
            resources: Default::default(),
            diagnostic: Arc::new(|_, _, _| {}),
        }),
    })
}

#[tokio::test]
async fn all_nine_native_backends_report_selectable_simulated_identities_without_configuration_or_leases()
 {
    let Some(native) = native() else {
        eprintln!("Set REGAIN_TEST_WORKERS to exercise production discovery simulations");
        return;
    };
    let config = HubConfig::empty();
    let runtime = HubRuntime::build(
        config.clone(),
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let service = HubService::read_only(runtime.clone());
    for device in [
        NativeDevice::Caa,
        NativeDevice::Efw,
        NativeDevice::Eaf,
        NativeDevice::Fc3,
        NativeDevice::Falcon,
        NativeDevice::Ofp2,
        NativeDevice::Eta,
        NativeDevice::CameraDirect,
        NativeDevice::CameraSdk,
    ] {
        let target = Target::Native { device };
        let catalog = service
            .discover_local(target, config.revision)
            .await
            .unwrap_or_else(|error| panic!("{device:?}: {error:?}"));
        assert_eq!(catalog.target, target);
        assert_eq!(catalog.configuration_revision, config.revision);
        assert!(catalog.simulated && !catalog.incomplete);
        assert!(!catalog.entries.is_empty());
        for entry in catalog.entries {
            let SourceBackend::Native {
                device: found,
                identity,
                camera,
                ..
            } = entry.backend
            else {
                panic!()
            };
            assert_eq!(found, device);
            assert!(!identity.trim().is_empty());
            assert_eq!(identity, identity.to_ascii_lowercase());
            assert!(entry.registered_class.is_none() && entry.blocked_reason.is_none());
            if let Some(camera) = camera {
                assert_eq!(camera.model, entry.name);
                assert!(!camera.sdk_fallback);
            }
        }
        assert_eq!(runtime.active_connections(), 0);
        assert!(runtime.source_snapshots().is_empty());
        assert_eq!(service.configuration(), config);
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_revision_missing_workers_and_absent_camera_backend_fail_before_hardware() {
    let directory = tempfile::tempdir().unwrap();
    let native = NativeRuntime {
        directory: directory.path().into(),
        simulate: true,
        references: None,
        cameras: None,
    };
    let config = HubConfig::empty();
    let runtime = HubRuntime::build(
        config.clone(),
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let service = HubService::read_only(runtime.clone());
    let target = Target::Native {
        device: NativeDevice::Eaf,
    };
    for revision in [Uuid::nil(), Uuid::new_v4()] {
        assert_eq!(
            service
                .discover_local(target, revision)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(
        service
            .discover_local(target, config.revision)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        service
            .discover_local(
                Target::Native {
                    device: NativeDevice::CameraSdk
                },
                config.revision
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert!(discover(native, target, Uuid::nil(), vec![]).await.is_err());
    assert_eq!(runtime.active_connections(), 0);
    service.shutdown().await.unwrap();
}

#[test]
fn local_target_rejects_arbitrary_paths_arguments_and_invalid_architectures() {
    for value in [
        serde_json::json!({"kind":"native","device":"eaf","path":"COM3"}),
        serde_json::json!({"kind":"native","device":"unknown"}),
        serde_json::json!({"kind":"com","deviceType":"camera","bitness":"arm64"}),
        serde_json::json!({"kind":"com","deviceType":"camera","bitness":"x64","command":"setup"}),
    ] {
        assert!(serde_json::from_value::<Target>(value).is_err());
    }
}
