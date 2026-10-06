//! Production accessory workers, explicitly in simulation; never USB hardware.
use regain_hub::{
    config::{NativeDevice, SourceBackend, SourceConfig},
    native::{NativeAccessoryBackend, NativeRuntime},
    parameters::PollPolicy,
    safety::MonotonicClock,
    source::{Backend, ErrorKind, SourceHandle, Values},
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;

fn runtime() -> Option<NativeRuntime> {
    let Some(directory) = std::env::var_os("REGAIN_TEST_WORKERS") else {
        eprintln!(
            "Native worker integration requires REGAIN_TEST_WORKERS pointing to built production workers"
        );
        return None;
    };
    let directory = PathBuf::from(directory);
    assert!(
        directory
            .join(format!("regain-device{}", std::env::consts::EXE_SUFFIX))
            .is_file()
    );
    Some(NativeRuntime {
        directory,
        simulate: true,
    })
}
fn config(device: NativeDevice, identity: &str) -> SourceConfig {
    SourceConfig {
        id: Uuid::new_v4(),
        label: "Explicit native simulation".into(),
        backend: SourceBackend::Native {
            device,
            identity: identity.into(),
        },
        polling: PollPolicy {
            poll_seconds: 0.2,
            request_timeout_seconds: 5.0,
            ..PollPolicy::default()
        },
    }
}

#[tokio::test]
async fn setup_inspection_reuses_native_property_maps_and_marks_simulation() {
    let Some(native) = runtime() else {
        return;
    };
    for (device, serial) in [
        (NativeDevice::Caa, "0102030405060708"),
        (NativeDevice::Efw, "0102030405060708"),
        (NativeDevice::Eaf, "0102030405060709"),
        (NativeDevice::Fc3, "00:00:00:00:00:03"),
        (NativeDevice::Falcon, "FALCON-SIMULATION"),
        (NativeDevice::Ofp2, "SIM-OFP2"),
        (NativeDevice::Eta, "SIMULATION"),
    ] {
        let mut cfg = regain_hub::config::HubConfig::empty();
        cfg.sources.push(config(device, serial));
        let source = cfg.sources[0].id;
        let hub = regain_hub::runtime::HubRuntime::build(
            cfg,
            &native,
            &regain_hub::factory::NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let report = hub.inspect_source(source, 0, 4).await.unwrap();
        assert_eq!(report.simulation, Some(true));
        let regain_hub::capabilities::Capabilities::Native { properties } = report.capabilities
        else {
            panic!("Expected native capability map")
        };
        assert!(!properties.is_empty());
        for property in &properties {
            if property.property != "temperature" {
                assert!(
                    matches!(
                        property.reading,
                        regain_hub::capabilities::Probe::Observed { .. }
                    ),
                    "{device:?}: {property:?}"
                );
            }
            if property.property == "ismoving" {
                assert!(!property.scalar_mapping);
            }
        }
        if device == NativeDevice::Eta {
            assert!(!properties.iter().any(|p| p.property == "temperature"));
        }
        hub.shutdown().await.unwrap();
    }
}
#[test]
fn construction_does_no_io_and_cameras_require_their_supervisor() {
    let runtime = NativeRuntime {
        directory: PathBuf::from("nonexistent-worker-directory"),
        simulate: false,
    };
    assert!(
        NativeAccessoryBackend::new(
            &config(NativeDevice::Fc3, "selected serial"),
            runtime.clone()
        )
        .is_ok()
    );
    for device in [NativeDevice::CameraDirect, NativeDevice::CameraSdk] {
        let result = NativeAccessoryBackend::new(&config(device, "camera"), runtime.clone());
        assert!(matches!(result, Err(error) if error.kind == ErrorKind::Unsupported));
    }
    for identity in ["", "\nserial", "serial\0", " \t"] {
        assert!(
            NativeAccessoryBackend::new(&config(NativeDevice::Fc3, identity), runtime.clone())
                .is_err()
        );
    }
}

#[tokio::test]
async fn all_native_accessories_verify_identity_map_samples_and_validate_motion() {
    let Some(runtime) = runtime() else {
        return;
    };
    for (device, serial) in [
        (NativeDevice::Caa, "0102030405060708"),
        (NativeDevice::Efw, "0102030405060708"),
        (NativeDevice::Eaf, "0102030405060709"),
        (NativeDevice::Fc3, "00:00:00:00:00:03"),
        (NativeDevice::Falcon, "FALCON-SIMULATION"),
        (NativeDevice::Ofp2, "SIM-OFP2"),
        (NativeDevice::Eta, "SIMULATION"),
    ] {
        let mut backend =
            NativeAccessoryBackend::new(&config(device, serial), runtime.clone()).unwrap();
        assert!(backend.simulated());
        backend.connect().await.unwrap();
        assert_eq!(
            backend
                .read("identity".into(), Values::new())
                .await
                .unwrap()["serial"],
            serial
        );
        assert_eq!(
            backend
                .read("identity".into(), Values::new())
                .await
                .unwrap()["simulation"],
            true
        );
        let initial = backend.sample().await.unwrap();
        assert!(!initial.values.is_empty(), "{device:?}");
        assert!(
            initial.errors.keys().all(|key| key == "temperature"),
            "{device:?}: {:?}",
            initial.errors
        );
        assert_eq!(
            backend
                .read("notaproperty".into(), Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
        let params = Values::from([("Position".into(), json!(-1))]);
        let member = if device == NativeDevice::Efw {
            "position"
        } else if matches!(device, NativeDevice::Caa | NativeDevice::Falcon) {
            "moveabsolute"
        } else {
            "move"
        };
        if device != NativeDevice::Ofp2 {
            assert_eq!(
                backend.write(member.into(), params).await.unwrap_err().kind,
                ErrorKind::InvalidValue
            );
            let target = if device == NativeDevice::Efw {
                1.0
            } else {
                initial.values["position"].as_f64().unwrap() + 2.0
            };
            let target_value = if matches!(device, NativeDevice::Caa | NativeDevice::Falcon) {
                json!(target)
            } else {
                json!(target as i64)
            };
            backend
                .write(
                    member.into(),
                    Values::from([("Position".into(), target_value)]),
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(8), async {
                loop {
                    let samples = backend.sample().await.unwrap();
                    if samples.values.get("ismoving") != Some(&json!(true))
                        && (samples.values["position"].as_f64().unwrap() - target).abs() < 0.11
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .unwrap();
            if device == NativeDevice::Eta {
                assert_eq!(
                    backend
                        .write("halt".into(), Values::new())
                        .await
                        .unwrap_err()
                        .kind,
                    ErrorKind::Unsupported
                );
            } else if device != NativeDevice::Efw {
                backend.write("halt".into(), Values::new()).await.unwrap();
            }
        } else {
            assert_eq!(
                backend
                    .write(
                        "calibratoron".into(),
                        Values::from([("Brightness".into(), json!(4097))])
                    )
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::InvalidValue
            );
            for brightness in [0, 128, 4096] {
                backend
                    .write(
                        "calibratoron".into(),
                        Values::from([("Brightness".into(), json!(brightness))]),
                    )
                    .await
                    .unwrap();
                let samples = backend.sample().await.unwrap();
                assert_eq!(samples.values["brightness"], brightness);
                assert_eq!(samples.values["calibratorstate"], 3);
            }
            backend
                .write("calibratoroff".into(), Values::new())
                .await
                .unwrap();
            assert_eq!(backend.sample().await.unwrap().values["calibratorstate"], 1);
        }
        backend.disconnect().await.unwrap();
        assert_eq!(
            backend
                .read("identity".into(), Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Disconnected
        );
    }
}

#[tokio::test]
async fn unavailable_identity_never_falls_back_to_another_device_or_simulator() {
    let Some(runtime) = runtime() else {
        return;
    };
    let mut backend =
        NativeAccessoryBackend::new(&config(NativeDevice::Fc3, "missing serial"), runtime).unwrap();
    assert!(backend.connect().await.is_err());
    assert!(
        backend.connect().await.is_err(),
        "Failed connection cannot leave a usable worker slot"
    );
    assert_eq!(
        backend
            .read("identity".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Disconnected
    );
    let runtime = NativeRuntime {
        directory: PathBuf::from("nonexistent-worker-directory"),
        simulate: false,
    };
    let mut backend =
        NativeAccessoryBackend::new(&config(NativeDevice::Fc3, "selected serial"), runtime)
            .unwrap();
    assert_eq!(
        backend.connect().await.unwrap_err().kind,
        ErrorKind::Permanent
    );
    assert!(!backend.simulated());
}

#[tokio::test]
async fn native_source_actor_shares_leases_and_preserves_other_clients_on_disconnect() {
    let Some(runtime) = runtime() else {
        return;
    };
    let config = config(NativeDevice::Fc3, "00:00:00:00:00:03");
    let backend = NativeAccessoryBackend::new(&config, runtime).unwrap();
    let source = SourceHandle::spawn(
        config.id,
        Uuid::new_v4(),
        config.polling,
        Box::new(backend),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let initial = source.acquire(first).await.unwrap();
    assert!(initial.transport_connected);
    let shared = source.acquire(second).await.unwrap();
    assert_eq!(shared.generation, initial.generation);
    assert_eq!(shared.lease_count, 2);
    source.control(first, true).await.unwrap();
    assert_eq!(
        source
            .write(
                second,
                "move",
                Values::from([("Position".into(), json!(50))])
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    source
        .write(
            first,
            "move",
            Values::from([("Position".into(), json!(50))]),
        )
        .await
        .unwrap();
    source.release(first).await.unwrap();
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(source.snapshot().generation, initial.generation);
    assert!(
        source
            .read(second, "temperature", Values::new())
            .await
            .unwrap()
            .is_number()
    );
    source.release(second).await.unwrap();
    assert!(!source.snapshot().transport_connected);
    assert!(source.snapshot().values.is_empty());
}

#[tokio::test]
async fn native_efw_calibration_preserves_provisional_slots_until_complete() {
    let Some(runtime) = runtime() else {
        return;
    };
    let mut backend =
        NativeAccessoryBackend::new(&config(NativeDevice::Efw, "0102030405060708"), runtime)
            .unwrap();
    backend.connect().await.unwrap();
    let slots = backend.sample().await.unwrap().values["slots"].clone();
    backend
        .write("calibrate".into(), Values::new())
        .await
        .unwrap();
    let active = backend.sample().await.unwrap();
    assert_eq!(active.values["position"], -1);
    assert_eq!(active.values["slots"], slots);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let sample = backend.sample().await.unwrap();
            assert_eq!(sample.values["slots"], slots);
            if sample.values["position"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    backend.disconnect().await.unwrap();
}

#[tokio::test]
async fn native_panel_cover_moves_and_halts_through_the_existing_coordinator() {
    let Some(runtime) = runtime() else {
        return;
    };
    let mut backend =
        NativeAccessoryBackend::new(&config(NativeDevice::Ofp2, "SIM-OFP2"), runtime).unwrap();
    backend.connect().await.unwrap();
    for (command, target) in [("opencover", 3), ("closecover", 1)] {
        backend.write(command.into(), Values::new()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while backend.sample().await.unwrap().values["coverstate"] != target {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
    }
    backend
        .write("opencover".into(), Values::new())
        .await
        .unwrap();
    backend
        .write("haltcover".into(), Values::new())
        .await
        .unwrap();
    assert_ne!(backend.sample().await.unwrap().values["coverstate"], 2);
    backend.disconnect().await.unwrap();
}
