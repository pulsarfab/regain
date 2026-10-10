use regain_core::RecoveryOptions;
use regain_hub::{
    camera::config::{CameraRecovery, NativeCameraConfig},
    config::{ConfigStore, HubConfig, NativeDevice, SourceBackend, SourceConfig},
};
use serde_json::{Value, json};
use uuid::Uuid;

fn source(device: &str, camera: Value) -> SourceConfig {
    serde_json::from_value(json!({"id":Uuid::new_v4(),"label":"Private camera",
        "backend":{"kind":"native","device":device,"identity":"PRIVATE-CAMERA","camera":camera}}))
    .unwrap()
}
fn config(source: SourceConfig) -> HubConfig {
    let mut config = HubConfig::empty();
    config.sources.push(source);
    config
}

#[test]
fn sparse_camera_recovery_preserves_core_values_and_binds_exact_identity_without_io() {
    let cfg = config(source(
        "camera-direct",
        json!({"model":"ZWO ASI585MM Pro","sdkFallback":true,
        "recovery":{"maxRetries":0,"directReadRetries":5,"usbResetAfterFailures":2,"usbPortCycle":true}}),
    ));
    assert!(cfg.validate().is_empty());
    let SourceBackend::Native {
        camera: Some(camera),
        identity,
        ..
    } = &cfg.sources[0].backend
    else {
        unreachable!()
    };
    let expected = RecoveryOptions {
        max_retries: 0,
        direct_read_retries: 5,
        usb_reset_after_failures: 2,
        usb_port_cycle: true,
        ..Default::default()
    };
    assert_eq!(camera.recovery.0, expected);
    let selection = camera.selection(identity, true);
    assert_eq!(selection.name, "ZWO ASI585MM Pro");
    assert_eq!(selection.serial.as_deref(), Some("PRIVATE-CAMERA"));
    assert!(selection.direct && selection.sdk_fallback);
    assert_eq!(selection.recovery, expected);
    let default: CameraRecovery = serde_json::from_value(json!({})).unwrap();
    assert_eq!(default.0, RecoveryOptions::default());
}

#[test]
fn new_hub_recovery_rejects_typos_and_wrong_types_without_changing_legacy_loading() {
    for invalid in [
        json!({"maxRetires":0}),
        json!({"maxRetries":false}),
        json!({"directReadRetries":1.5}),
        json!({"usbPortCycle":0}),
        json!({"commandTimeoutSeconds":"5"}),
        json!(null),
        json!([]),
    ] {
        assert!(serde_json::from_value::<CameraRecovery>(invalid).is_err());
    }
    assert!(serde_json::from_value::<RecoveryOptions>(json!({"maxRetires":0})).is_ok());
    assert!(
        serde_json::from_value::<NativeCameraConfig>(json!({"model":"test","sdkFallBack":true}))
            .is_err()
    );
}

#[test]
fn invalid_recovery_reports_shared_field_paths_and_retains_strict_positive_limits() {
    let cfg = config(source(
        "camera-direct",
        json!({"model":"ZWO ASI585MM Pro",
        "recovery":{"maxRetries":21,"maximumRetryExposureSeconds":86401,"reconnectDelaySeconds":0,
        "commandTimeoutSeconds":3601,"coolingStableSamples":0,"readyFrameDownloadRetries":6}}),
    ));
    let errors = cfg.validate();
    assert_eq!(errors.len(), 6, "{errors:?}");
    for key in [
        "maxRetries",
        "maximumRetryExposureSeconds",
        "reconnectDelaySeconds",
        "commandTimeoutSeconds",
        "coolingStableSamples",
        "readyFrameDownloadRetries",
    ] {
        assert!(errors.iter().any(|e| e.path
            == format!("sources[0].backend.camera.recovery.{key}")
            && e.code == "range"));
    }
    let tiny = config(source(
        "camera-sdk",
        json!({"model":"ZWO ASI585MM Pro",
        "recovery":{"reconnectDelaySeconds":f64::MIN_POSITIVE,"maximumRetryExposureSeconds":0}}),
    ));
    assert!(tiny.validate().is_empty());
    let recovery = CameraRecovery(RecoveryOptions {
        download_timeout_seconds: f64::NAN,
        ..Default::default()
    });
    assert_eq!(recovery.validate()[0].path, "downloadTimeoutSeconds");
}

#[test]
fn camera_configuration_is_required_and_native_class_specific() {
    for device in ["camera-direct", "camera-sdk"] {
        let cfg = config(source(device, Value::Null));
        assert!(
            cfg.validate()
                .iter()
                .any(|e| e.path == "sources[0].backend.camera" && e.code == "required")
        );
    }
    for device in ["efw", "eaf", "caa", "fc3", "falcon", "ofp2", "eta"] {
        let cfg = config(source(device, json!({"model":"ZWO ASI585MM Pro"})));
        assert!(
            cfg.validate()
                .iter()
                .any(|e| e.path == "sources[0].backend.camera" && e.code == "type")
        );
    }
    let sdk = config(source(
        "camera-sdk",
        json!({"model":"ZWO ASI585MM Pro","sdkFallback":true}),
    ));
    assert!(
        sdk.validate()
            .iter()
            .any(|e| e.path == "sources[0].backend.camera.sdkFallback")
    );
    for model in [
        "".into(),
        "   ".into(),
        "test\nname".into(),
        "a".repeat(201),
    ] {
        let cfg = config(source("camera-direct", json!({"model":model})));
        assert!(
            cfg.validate()
                .iter()
                .any(|e| e.path == "sources[0].backend.camera.model")
        );
    }
}

#[test]
fn native_camera_recovery_round_trips_through_atomic_store_without_identity_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.json");
    let cfg = config(source("camera-direct", json!({"model":"ZWO ASI585MM Pro"})));
    let store = ConfigStore::new(Some(path.clone()), cfg).unwrap();
    let before = store.snapshot();
    let mut next = before.clone();
    let SourceBackend::Native {
        camera: Some(camera),
        ..
    } = &mut next.sources[0].backend
    else {
        unreachable!()
    };
    camera.recovery.0.max_retries = 0;
    camera.recovery.0.direct_read_retries = 5;
    let saved = store.apply(next.revision, next, false).unwrap();
    let loaded = ConfigStore::load(&path).unwrap().snapshot();
    assert_eq!(saved, loaded);
    assert_eq!(loaded.sources[0].id, before.sources[0].id);
    assert_eq!(loaded.sources[0].polling, before.sources[0].polling);
    assert_eq!(loaded.identities, before.identities);
    assert_ne!(loaded.revision, before.revision);
}

#[test]
fn direct_and_sdk_configuration_cannot_claim_one_physical_camera_twice() {
    let direct = source("camera-direct", json!({"model":"ZWO ASI585MM Pro"}));
    let mut cfg = config(direct);
    cfg.sources
        .push(source("camera-sdk", json!({"model":"ZWO ASI585MM Pro"})));
    assert!(cfg.validate().iter().any(|e| e.code == "duplicate"));
    let SourceBackend::Native { identity, .. } = &mut cfg.sources[1].backend else {
        unreachable!()
    };
    *identity = "OTHER-CAMERA".into();
    assert!(cfg.validate().is_empty());
    let SourceBackend::Native { device, .. } = &mut cfg.sources[1].backend else {
        unreachable!()
    };
    *device = NativeDevice::CameraDirect;
    assert!(cfg.validate().is_empty());
}
