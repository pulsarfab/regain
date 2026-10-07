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
        references: None,
    })
}
fn config(device: NativeDevice, identity: &str) -> SourceConfig {
    SourceConfig {
        id: Uuid::new_v4(),
        label: "Explicit native simulation".into(),
        backend: SourceBackend::Native {
            camera: None,
            device,
            identity: identity.into(),
            filter_wheel: None,
        },
        polling: PollPolicy {
            poll_seconds: 0.2,
            request_timeout_seconds: 5.0,
            ..PollPolicy::default()
        },
    }
}

#[tokio::test]
async fn runtime_native_wheel_preserves_metadata_cache_and_shared_worker_ownership() {
    native_wheel_runtime(false).await;
}
#[tokio::test]
async fn nested_native_wheel_preserves_explicit_simulation_metadata_motion_and_leases() {
    native_wheel_runtime(true).await;
}
async fn native_wheel_runtime(nested: bool) {
    use regain_hub::{
        config::{DeviceType, HubConfig, OutputConfig, VirtualDevice},
        diagnostics::{Diagnostics, Reading},
        factory::NoCredentials,
        filterwheel::{FilterWheelProperty, NativeFilterWheelMetadata},
        runtime::HubRuntime,
    };
    let Some(native) = runtime() else {
        return;
    };
    let mut source = config(NativeDevice::Efw, "0102030405060708");
    let metadata = NativeFilterWheelMetadata {
        names: ["L", "R", "G", "B", "Hα", "", "SII"]
            .into_iter()
            .map(String::from)
            .collect(),
        focus_offsets: vec![0, -12, 17, 30, 45, 52, 67],
    };
    let SourceBackend::Native { filter_wheel, .. } = &mut source.backend else {
        unreachable!()
    };
    *filter_wheel = Some(metadata.clone());
    let mut config = HubConfig::empty();
    config.sources.push(source.clone());
    for number in [4, 17] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Simulated wheel {number}"),
            device: VirtualDevice::Proxy {
                source: source.id,
                device_type: DeviceType::FilterWheel,
            },
        });
    }
    if nested {
        let mut previous = config.outputs[0].id;
        for number in [42, 91] {
            let virtual_source = Uuid::new_v4();
            config.sources.push(SourceConfig {
                id: virtual_source,
                label: "Nested native wheel input".into(),
                polling: source.polling.clone(),
                backend: SourceBackend::Virtual { output: previous },
            });
            previous = Uuid::new_v4();
            config.outputs.push(OutputConfig {
                id: previous,
                number,
                label: "Nested native wheel output".into(),
                device: VirtualDevice::Proxy {
                    source: virtual_source,
                    device_type: DeviceType::FilterWheel,
                },
            });
        }
    }
    let selected = if nested {
        config.outputs.last().unwrap().id
    } else {
        config.outputs[1].id
    };
    let host = HubRuntime::build(
        config.clone(),
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    assert!(host.outputs().iter().all(|output| output.simulated));
    assert!(
        host.source_snapshots()
            .iter()
            .all(|state| state.simulated && state.lease_count == 0)
    );
    assert_eq!(
        host.outputs()
            .iter()
            .map(|output| output.number)
            .collect::<Vec<_>>(),
        if nested {
            vec![4, 17, 42, 91]
        } else {
            vec![4, 17]
        }
    );
    assert_eq!(host.active_connections(), 0);
    let Diagnostics::FilterWheel { health, properties } = host
        .output_status(config.outputs[0].id, 0, 32)
        .unwrap()
        .diagnostics
    else {
        panic!()
    };
    assert_eq!(health.lease_count, 0);
    assert!(
        properties
            .iter()
            .all(|property| matches!(property.sample, Reading::Unavailable { .. }))
    );
    let first = host.client();
    let second = host.client();
    first.connect(config.outputs[0].id).await.unwrap();
    second.connect(selected).await.unwrap();
    let connection = second.connection(selected).unwrap();
    let wheel = connection.filterwheel().unwrap();
    assert_eq!(host.source_snapshot(source.id).unwrap().lease_count, 2);
    assert_eq!(
        wheel.property(FilterWheelProperty::Names).await.unwrap(),
        json!(metadata.names)
    );
    assert_eq!(
        wheel
            .property(FilterWheelProperty::FocusOffsets)
            .await
            .unwrap(),
        json!(metadata.focus_offsets)
    );
    first
        .connection(config.outputs[0].id)
        .unwrap()
        .filterwheel()
        .unwrap()
        .move_to(6)
        .await
        .unwrap();
    assert_eq!(wheel.position().await.unwrap(), 6);
    tokio::time::timeout(Duration::from_secs(3),async {
        loop {
            let Diagnostics::FilterWheel {health,properties} =
                host.output_status(selected,0,32).unwrap().diagnostics else {panic!()};
            if serde_json::to_value(&properties[2].sample).unwrap()["reading"]["value"]["value"] == 6 {
                assert!(host.source_snapshots().iter().all(|state|state.simulated)); assert_eq!(health.lease_count,if nested {1}else{2});
                assert_eq!(serde_json::to_value(&properties[0].sample).unwrap()["reading"]["value"]["value"],json!(metadata.names));
                assert_eq!(serde_json::to_value(&properties[1].sample).unwrap()["reading"]["value"]["value"],json!(metadata.focus_offsets));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    first.close();
    tokio::time::timeout(Duration::from_secs(3), async {
        while host.source_snapshot(source.id).unwrap().lease_count != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(connection.connected());
    assert_eq!(wheel.position().await.unwrap(), 6);
    drop(connection);
    second.close();
    tokio::time::timeout(Duration::from_secs(3), async {
        while host.source_snapshot(source.id).unwrap().transport_connected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let fresh = host.client();
    fresh.connect(selected).await.unwrap();
    let connection = fresh.connection(selected).unwrap();
    assert_eq!(
        connection
            .filterwheel()
            .unwrap()
            .property(FilterWheelProperty::Names)
            .await
            .unwrap(),
        json!(metadata.names)
    );
    assert_eq!(
        connection
            .filterwheel()
            .unwrap()
            .property(FilterWheelProperty::FocusOffsets)
            .await
            .unwrap(),
        json!(metadata.focus_offsets)
    );
    drop(connection);
    fresh.close();
    host.shutdown().await.unwrap();
    assert!(
        host.source_snapshots()
            .iter()
            .all(|state| state.lease_count == 0 && !state.transport_connected)
    );
}

#[tokio::test]
async fn runtime_native_rotator_proxies_share_verified_workers_and_cached_typed_health() {
    native_rotator_runtime(false).await;
}
#[tokio::test]
async fn nested_native_rotators_preserve_explicit_simulation_shared_reference_and_leases() {
    native_rotator_runtime(true).await;
}
async fn native_rotator_runtime(nested: bool) {
    use regain_hub::{
        config::{DeviceType, HubConfig, OutputConfig, SourceConfig, VirtualDevice},
        diagnostics::Diagnostics,
        factory::NoCredentials,
        native_reference::NativeReferenceStore,
        rotator::RotatorProperty,
        runtime::HubRuntime,
    };
    let Some(mut native) = runtime() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    native.references = Some(
        NativeReferenceStore::at_directory(&directory.path().join("references"), &"d".repeat(64))
            .unwrap(),
    );
    for (device, identity) in [
        (NativeDevice::Caa, "0102030405060708"),
        (NativeDevice::Falcon, "FALCON-SIMULATION"),
    ] {
        let source = config(device, identity);
        let mut config = HubConfig::empty();
        config.sources.push(source.clone());
        for number in [5, 27] {
            config.outputs.push(OutputConfig {
                id: Uuid::new_v4(),
                number,
                label: format!("Native rotator {number}"),
                device: VirtualDevice::Proxy {
                    source: source.id,
                    device_type: DeviceType::Rotator,
                },
            });
        }
        if nested {
            let virtual_source = Uuid::new_v4();
            config.sources.push(SourceConfig {
                id: virtual_source,
                label: "Nested native rotator input".into(),
                polling: source.polling.clone(),
                backend: SourceBackend::Virtual {
                    output: config.outputs[0].id,
                },
            });
            config.outputs[1].device = VirtualDevice::Proxy {
                source: virtual_source,
                device_type: DeviceType::Rotator,
            };
        }
        let host = HubRuntime::build(
            config.clone(),
            &native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        assert_eq!(host.active_connections(), 0);
        assert!(host.outputs().iter().all(|item| item.simulated));
        assert!(host.source_snapshots().iter().all(|item| item.simulated));
        assert_eq!(
            host.outputs()
                .iter()
                .map(|item| item.number)
                .collect::<Vec<_>>(),
            vec![5, 27]
        );
        let inactive = host.output_status(config.outputs[0].id, 0, 32).unwrap();
        assert!(inactive.simulated);
        assert_eq!(inactive.total, 7);
        assert_eq!(host.source_snapshot(source.id).unwrap().lease_count, 0);
        let first = host.client();
        let second = host.client();
        first.connect(config.outputs[0].id).await.unwrap();
        second.connect(config.outputs[1].id).await.unwrap();
        let connection = second.connection(config.outputs[1].id).unwrap();
        assert_eq!(host.source_snapshot(source.id).unwrap().lease_count, 2);
        first
            .connection(config.outputs[0].id)
            .unwrap()
            .rotator()
            .unwrap()
            .sync(42.5)
            .await
            .unwrap();
        assert_eq!(
            connection
                .rotator()
                .unwrap()
                .property(RotatorProperty::Position)
                .await
                .unwrap(),
            42.5
        );
        assert_eq!(
            connection
                .rotator()
                .unwrap()
                .property(RotatorProperty::TargetPosition)
                .await
                .unwrap(),
            42.5
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let Diagnostics::Rotator { properties, .. } = host
                    .output_status(config.outputs[1].id, 0, 32)
                    .unwrap()
                    .diagnostics
                else {
                    panic!()
                };
                if serde_json::to_value(&properties[3].sample).unwrap()["reading"]["value"]["value"]
                    == 42.5
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        first.close();
        tokio::time::timeout(Duration::from_secs(3), async {
            while host.source_snapshot(source.id).unwrap().lease_count != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(connection.connected());
        assert_eq!(
            connection
                .rotator()
                .unwrap()
                .property(RotatorProperty::Position)
                .await
                .unwrap(),
            42.5
        );
        drop(connection);
        second.close();
        host.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn typed_native_rotators_preserve_reference_across_actual_worker_recreation() {
    use regain_hub::{
        native_reference::NativeReferenceStore,
        rotator::{RotatorController, RotatorProperty},
    };
    let Some(mut native) = runtime() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    native.references = Some(
        NativeReferenceStore::at_directory(&directory.path().join("references"), &"c".repeat(64))
            .unwrap(),
    );
    for (device, identity) in [
        (NativeDevice::Caa, "0102030405060708"),
        (NativeDevice::Falcon, "FALCON-SIMULATION"),
    ] {
        let config = config(device, identity);
        let source = SourceHandle::spawn(
            config.id,
            Uuid::new_v4(),
            config.polling.clone(),
            Box::new(NativeAccessoryBackend::new(&config, native.clone()).unwrap()),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let controller = RotatorController::new(source.clone(), Duration::from_secs(10)).unwrap();
        let first = controller.connect().await.unwrap();
        let second = controller.connect().await.unwrap();
        assert_eq!(first.generation(), second.generation());
        assert!(source.snapshot().simulated);
        assert!(first.capabilities().await.unwrap().can_reverse);
        assert_eq!(
            first.property(RotatorProperty::StepSize).await.unwrap(),
            if device == NativeDevice::Caa {
                0.02
            } else {
                0.01
            }
        );
        let mechanical = first
            .property(RotatorProperty::MechanicalPosition)
            .await
            .unwrap();
        first.sync(42.5).await.unwrap();
        assert_eq!(
            second.property(RotatorProperty::Position).await.unwrap(),
            42.5
        );
        assert_eq!(
            second
                .property(RotatorProperty::TargetPosition)
                .await
                .unwrap(),
            42.5
        );
        first.set_reverse(true).await.unwrap();
        assert_eq!(
            second.property(RotatorProperty::Position).await.unwrap(),
            42.5
        );
        assert_eq!(
            second.property(RotatorProperty::Reverse).await.unwrap(),
            true
        );
        first.set_reverse(false).await.unwrap();
        assert_eq!(
            second
                .property(RotatorProperty::MechanicalPosition)
                .await
                .unwrap(),
            mechanical
        );
        drop(first);
        assert_eq!(
            second.property(RotatorProperty::Position).await.unwrap(),
            42.5
        );
        drop(second);
        source.shutdown().await.unwrap();
        // New backend and worker, same saved source/hardware/simulation binding.
        let mut fresh = NativeAccessoryBackend::new(&config, native.clone()).unwrap();
        fresh.connect().await.unwrap();
        assert_eq!(
            fresh.read("position".into(), Values::new()).await.unwrap(),
            42.5
        );
        assert_eq!(
            fresh
                .read("targetposition".into(), Values::new())
                .await
                .unwrap(),
            42.5
        );
        assert_eq!(
            fresh
                .read("mechanicalposition".into(), Values::new())
                .await
                .unwrap(),
            mechanical
        );
        assert_eq!(
            fresh.read("reverse".into(), Values::new()).await.unwrap(),
            false
        );
        fresh
            .write(
                "moveabsolute".into(),
                Values::from([("Position".into(), json!(43.5))]),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while fresh.read("ismoving".into(), Values::new()).await.unwrap() == true {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let angle = fresh
            .read("position".into(), Values::new())
            .await
            .unwrap()
            .as_f64()
            .unwrap();
        assert!((angle - 43.5).abs() < 0.03);
        fresh.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn hardware_direction_change_does_not_replay_reverse_or_trust_old_reference_on_connect() {
    use regain_hub::native_reference::NativeReferenceStore;
    let Some(mut native) = runtime() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    native.references = Some(
        NativeReferenceStore::at_directory(&directory.path().join("references"), &"d".repeat(64))
            .unwrap(),
    );
    for (device, identity) in [
        (NativeDevice::Caa, "0102030405060708"),
        (NativeDevice::Falcon, "FALCON-SIMULATION"),
    ] {
        let config = config(device, identity);
        let mut backend = NativeAccessoryBackend::new(&config, native.clone()).unwrap();
        backend.connect().await.unwrap();
        let mechanical = backend
            .read("mechanicalposition".into(), Values::new())
            .await
            .unwrap();
        backend
            .write(
                "sync".into(),
                Values::from([("Position".into(), json!(42.5))]),
            )
            .await
            .unwrap();
        backend
            .write(
                "reverse".into(),
                Values::from([("Reverse".into(), json!(true))]),
            )
            .await
            .unwrap();
        backend.disconnect().await.unwrap();
        // A fresh simulated device starts in its default direction. This is an
        // explicit external change, not evidence of firmware persistence.
        let mut fresh = NativeAccessoryBackend::new(&config, native.clone()).unwrap();
        fresh.connect().await.unwrap();
        assert_eq!(
            fresh.read("reverse".into(), Values::new()).await.unwrap(),
            false
        );
        assert_eq!(
            fresh
                .read("mechanicalposition".into(), Values::new())
                .await
                .unwrap(),
            mechanical
        );
        for member in ["position", "targetposition"] {
            assert_eq!(
                fresh
                    .read(member.into(), Values::new())
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::Unavailable
            );
        }
        assert_eq!(
            fresh
                .write(
                    "moveabsolute".into(),
                    Values::from([("Position".into(), json!(30.0))])
                )
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            fresh
                .write(
                    "reverse".into(),
                    Values::from([("Reverse".into(), json!(true))])
                )
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
        fresh
            .write(
                "sync".into(),
                Values::from([("Position".into(), json!(95.0))]),
            )
            .await
            .unwrap();
        assert_eq!(
            fresh.read("position".into(), Values::new()).await.unwrap(),
            95.0
        );
        assert_eq!(
            fresh
                .read("mechanicalposition".into(), Values::new())
                .await
                .unwrap(),
            mechanical
        );
        fresh.disconnect().await.unwrap();
        let mut last = NativeAccessoryBackend::new(&config, native.clone()).unwrap();
        last.connect().await.unwrap();
        assert_eq!(
            last.read("position".into(), Values::new()).await.unwrap(),
            95.0
        );
        last.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn native_reference_storage_failure_precedes_dispatch_and_never_falls_back() {
    use regain_hub::native_reference::NativeReferenceStore;
    let Some(mut native) = runtime() else {
        return;
    };
    let config = config(NativeDevice::Caa, "0102030405060708");
    let mut backend = NativeAccessoryBackend::new(&config, native.clone()).unwrap();
    backend.connect().await.unwrap();
    let initial = backend
        .read("position".into(), Values::new())
        .await
        .unwrap();
    assert_eq!(
        backend
            .write(
                "sync".into(),
                Values::from([("Position".into(), json!(42.5))])
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        backend
            .read("position".into(), Values::new())
            .await
            .unwrap(),
        initial
    );
    backend.disconnect().await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().join("references");
    std::fs::create_dir(&base).unwrap();
    std::fs::write(base.join("e".repeat(64)), "invalid private directory").unwrap();
    native.references = Some(NativeReferenceStore::at_directory(&base, &"e".repeat(64)).unwrap());
    native.directory = directory.path().join("nonexistent-workers");
    let mut backend = NativeAccessoryBackend::new(&config, native).unwrap();
    assert_eq!(
        backend.connect().await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        backend
            .read("identity".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Disconnected
    );
    assert_eq!(
        std::fs::read_to_string(base.join("e".repeat(64))).unwrap(),
        "invalid private directory"
    );
}

#[tokio::test]
async fn lost_reply_or_ignored_native_sync_retains_durable_uncertainty_after_source_recreation() {
    use regain_hub::{
        native_reference::NativeReferenceStore,
        rotator::{RotatorController, RotatorProperty},
    };
    let directory = tempfile::tempdir().unwrap();
    let fixture = directory.path().join("worker.rs");
    std::fs::write(
        &fixture,
        include_str!("fixtures/native_rotator_reference.rs"),
    )
    .unwrap();
    let executable = directory
        .path()
        .join(format!("regain-device{}", std::env::consts::EXE_SUFFIX));
    let output = std::process::Command::new("rustc")
        .args(["--edition=2021", "--crate-name", "native_reference_fixture"])
        .arg(&fixture)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for mode in ["lost", "ignored"] {
        std::fs::write(directory.path().join("commands.log"), "").unwrap();
        std::fs::write(directory.path().join("mode"), mode).unwrap();
        let native = NativeRuntime {
            directory: directory.path().to_owned(),
            simulate: true,
            references: Some(
                NativeReferenceStore::at_directory(
                    &directory.path().join("references"),
                    &"f".repeat(64),
                )
                .unwrap(),
            ),
        };
        let config = config(NativeDevice::Caa, "PRIVATE-REFERENCE");
        let source = SourceHandle::spawn(
            config.id,
            Uuid::new_v4(),
            config.polling.clone(),
            Box::new(NativeAccessoryBackend::new(&config, native.clone()).unwrap()),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let controller = RotatorController::new(source.clone(), Duration::from_secs(10)).unwrap();
        let first = controller.connect().await.unwrap();
        let second = controller.connect().await.unwrap();
        assert_eq!(
            first.sync(42.5).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("applied-offset")).unwrap(),
            if mode == "lost" { "-109.5" } else { "0" }
        );
        assert!(source.snapshot().write_uncertain);
        assert!(!first.connected());
        assert!(!second.connected());
        assert_eq!(
            second.sync(43.0).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        drop(first);
        drop(second);
        source.shutdown().await.unwrap();
        std::fs::write(directory.path().join("mode"), "normal").unwrap();
        let source = SourceHandle::spawn(
            config.id,
            Uuid::new_v4(),
            config.polling.clone(),
            Box::new(NativeAccessoryBackend::new(&config, native.clone()).unwrap()),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let controller = RotatorController::new(source.clone(), Duration::from_secs(10)).unwrap();
        let fresh = controller.connect().await.unwrap();
        assert_eq!(
            fresh
                .property(RotatorProperty::Position)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            fresh
                .property(RotatorProperty::TargetPosition)
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            fresh
                .property(RotatorProperty::MechanicalPosition)
                .await
                .unwrap(),
            152.0
        );
        assert_eq!(
            fresh.move_absolute(30.0).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        let trace = std::fs::read_to_string(directory.path().join("commands.log")).unwrap();
        assert_eq!(
            trace
                .lines()
                .filter(|line| line.contains("\"command\":\"sync\""))
                .count(),
            1
        );
        assert!(!trace.contains("restore-reference"));
        fresh.sync(95.0).await.unwrap();
        assert_eq!(
            fresh.property(RotatorProperty::Position).await.unwrap(),
            95.0
        );
        assert_eq!(
            fresh
                .property(RotatorProperty::MechanicalPosition)
                .await
                .unwrap(),
            152.0
        );
        drop(fresh);
        source.shutdown().await.unwrap();
        let mut restored = NativeAccessoryBackend::new(&config, native).unwrap();
        restored.connect().await.unwrap();
        assert_eq!(
            restored
                .read("position".into(), Values::new())
                .await
                .unwrap(),
            95.0
        );
        assert_eq!(
            restored
                .read("mechanicalposition".into(), Values::new())
                .await
                .unwrap(),
            152.0
        );
        restored.disconnect().await.unwrap();
        let trace = std::fs::read_to_string(directory.path().join("commands.log")).unwrap();
        assert_eq!(
            trace
                .lines()
                .filter(|line| line.contains("\"command\":\"sync\""))
                .count(),
            2
        );
        assert_eq!(
            trace
                .lines()
                .filter(|line| line.contains("restore-reference"))
                .count(),
            1
        );
        assert!(
            !trace.contains("move-")
                && !trace.contains("\"command\":\"reverse\"")
                && !trace.contains("\"command\":\"reference\"")
        );
    }
}

#[tokio::test]
async fn typed_focusers_use_production_workers_with_shared_explicit_simulation_leases() {
    let Some(native) = runtime() else {
        return;
    };
    for (device, identity) in [
        (NativeDevice::Eaf, "0102030405060709"),
        (NativeDevice::Fc3, "00:00:00:00:00:03"),
        (NativeDevice::Eta, "SIMULATION"),
    ] {
        let config = config(device, identity);
        let source = SourceHandle::spawn(
            config.id,
            Uuid::new_v4(),
            config.polling.clone(),
            Box::new(NativeAccessoryBackend::new(&config, native.clone()).unwrap()),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let controller =
            regain_hub::focuser::FocuserController::new(source.clone(), Duration::from_secs(10))
                .unwrap();
        let first = controller.connect().await.unwrap();
        let second = controller.connect().await.unwrap();
        assert!(source.snapshot().simulated);
        assert_eq!(first.generation(), second.generation());
        let capabilities = first.capabilities().await.unwrap();
        assert!(capabilities.absolute);
        assert!(!capabilities.temp_comp_available);
        assert!(!first.temp_comp().await.unwrap());
        if device == NativeDevice::Eta {
            assert_eq!(first.step_size().await.unwrap(), 1.0);
        } else {
            assert_eq!(
                first.step_size().await.unwrap_err().kind,
                ErrorKind::Unsupported
            );
        }
        assert_eq!(
            first.set_temp_comp(true).await.unwrap_err().kind,
            ErrorKind::Unsupported
        );
        let initial = first.position().await.unwrap();
        let target = initial
            .checked_add(10)
            .filter(|target| *target <= capabilities.max_step)
            .unwrap_or(initial - 10);
        first.move_to(target).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while second.is_moving().await.unwrap() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(second.position().await.unwrap(), target);
        if device == NativeDevice::Eta {
            assert_eq!(
                second.halt().await.unwrap_err().kind,
                ErrorKind::Unsupported
            );
        } else {
            second.halt().await.unwrap();
        }
        assert!(!source.snapshot().write_uncertain);
        drop(first);
        tokio::task::yield_now().await;
        assert!(second.position().await.is_ok());
        drop(second);
        let mut status = source.status();
        tokio::time::timeout(Duration::from_secs(5), async {
            while status.borrow_and_update().lease_count != 0 {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(!source.snapshot().transport_connected);
        source.shutdown().await.unwrap();
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

#[tokio::test]
async fn runtime_native_focuser_proxies_keep_stable_numbers_and_one_simulated_worker() {
    use regain_hub::{
        config::{DeviceType, HubConfig, OutputConfig, VirtualDevice},
        runtime::HubRuntime,
    };
    let Some(native) = runtime() else {
        return;
    };
    let mut config = HubConfig::empty();
    config
        .sources
        .push(self::config(NativeDevice::Fc3, "00:00:00:00:00:03"));
    let source_id = config.sources[0].id;
    for number in [4, 7] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Simulated focuser {number}"),
            device: VirtualDevice::Proxy {
                source: source_id,
                device_type: DeviceType::Focuser,
            },
        });
    }
    let runtime = HubRuntime::build(
        config.clone(),
        &native,
        &regain_hub::factory::NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    assert_eq!(
        runtime
            .outputs()
            .iter()
            .map(|output| output.number)
            .collect::<Vec<_>>(),
        vec![4, 7]
    );
    assert!(runtime.outputs().iter().all(|output| output.simulated));
    let first = runtime.client();
    let second = runtime.client();
    first.connect(config.outputs[0].id).await.unwrap();
    second.connect(config.outputs[1].id).await.unwrap();
    assert_eq!(runtime.source_snapshot(source_id).unwrap().lease_count, 2);
    let connection = first.connection(config.outputs[0].id).unwrap();
    let initial = connection.focuser().unwrap().position().await.unwrap();
    connection
        .focuser()
        .unwrap()
        .move_to(initial + 10)
        .await
        .unwrap();
    drop(connection);
    tokio::time::timeout(Duration::from_secs(5), async {
        while second
            .connection(config.outputs[1].id)
            .unwrap()
            .focuser()
            .unwrap()
            .is_moving()
            .await
            .unwrap()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        second
            .connection(config.outputs[1].id)
            .unwrap()
            .focuser()
            .unwrap()
            .position()
            .await
            .unwrap(),
        initial + 10
    );
    first.close();
    assert!(
        second
            .connection(config.outputs[1].id)
            .unwrap()
            .focuser()
            .unwrap()
            .connected()
    );
    second.close();
    runtime.shutdown().await.unwrap();
}
#[test]
fn construction_does_no_io_and_cameras_require_their_supervisor() {
    let runtime = NativeRuntime {
        directory: PathBuf::from("nonexistent-worker-directory"),
        simulate: false,
        references: None,
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
            initial
                .errors
                .keys()
                .all(|key| key == "temperature" || key == "stepsize"),
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
        references: None,
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
    let mut cfg = config(NativeDevice::Efw, "0102030405060708");
    let metadata = regain_hub::filterwheel::NativeFilterWheelMetadata {
        names: (1..=7).map(|slot| format!("Saved filter {slot}")).collect(),
        focus_offsets: vec![0, -12, 17, 30, 45, 52, 67],
    };
    let SourceBackend::Native { filter_wheel, .. } = &mut cfg.backend else {
        unreachable!()
    };
    *filter_wheel = Some(metadata.clone());
    let mut backend = NativeAccessoryBackend::new(&cfg, runtime).unwrap();
    backend.connect().await.unwrap();
    let slots = backend.sample().await.unwrap().values["slots"].clone();
    backend
        .write("calibrate".into(), Values::new())
        .await
        .unwrap();
    let active = backend.sample().await.unwrap();
    assert_eq!(active.values["position"], -1);
    assert_eq!(active.values["slots"], slots);
    assert_eq!(active.values["names"], json!(metadata.names));
    assert_eq!(active.values["focusoffsets"], json!(metadata.focus_offsets));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let sample = backend.sample().await.unwrap();
            assert_eq!(sample.values["slots"], slots);
            assert_eq!(sample.values["names"], json!(metadata.names));
            assert_eq!(sample.values["focusoffsets"], json!(metadata.focus_offsets));
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
    let moving = backend.sample().await.unwrap();
    assert_eq!(moving.values["coverstate"], 2);
    assert_eq!(moving.values["covermoving"], true);
    assert_eq!(moving.values["calibratorchanging"], false);
    assert_eq!(
        backend
            .write("closecover".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    backend
        .write("haltcover".into(), Values::new())
        .await
        .unwrap();
    let stopped = backend.sample().await.unwrap();
    assert_eq!(stopped.values["coverstate"], 4);
    assert_eq!(stopped.values["covermoving"], false);
    backend.disconnect().await.unwrap();
}

#[tokio::test]
async fn native_panel_controller_shares_motion_and_zero_on_without_disconnect_actuation() {
    use regain_hub::covercalibrator::{
        CoverCalibratorController, CoverCalibratorProperty as Property,
    };
    let Some(native) = runtime() else {
        return;
    };
    let cfg = config(NativeDevice::Ofp2, "SIM-OFP2");
    let backend = NativeAccessoryBackend::new(&cfg, native).unwrap();
    let source = SourceHandle::spawn(
        cfg.id,
        Uuid::new_v4(),
        cfg.polling,
        Box::new(backend),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let controller =
        CoverCalibratorController::new(source.clone(), Duration::from_secs(10)).unwrap();
    assert_eq!(source.snapshot().lease_count, 0);
    let first = controller.connect().await.unwrap();
    let second = controller.connect().await.unwrap();
    assert!(source.snapshot().simulated);
    assert_eq!(source.snapshot().lease_count, 2);
    assert_eq!(first.generation(), second.generation());
    assert_eq!(first.property(Property::MaxBrightness).await.unwrap(), 4096);
    assert_eq!(
        first.calibrator_on(4097).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    first.calibrator_on(0).await.unwrap();
    assert_eq!(second.property(Property::Brightness).await.unwrap(), 0);
    assert_eq!(second.property(Property::CalibratorState).await.unwrap(), 3);
    assert_eq!(
        second.property(Property::CalibratorChanging).await.unwrap(),
        false
    );
    first.close_cover().await.unwrap();
    assert_eq!(second.property(Property::CoverState).await.unwrap(), 2);
    assert_eq!(second.property(Property::CoverMoving).await.unwrap(), true);
    assert_eq!(second.open_cover().await.unwrap_err().kind, ErrorKind::Busy);
    assert!(!source.snapshot().write_uncertain);
    second.halt_cover().await.unwrap();
    assert_eq!(second.property(Property::CoverState).await.unwrap(), 4);
    assert_eq!(second.property(Property::CoverMoving).await.unwrap(), false);
    first.calibrator_on(17).await.unwrap();
    drop(first);
    tokio::time::timeout(Duration::from_secs(3), async {
        while source.snapshot().lease_count != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(second.connected());
    assert_eq!(second.property(Property::Brightness).await.unwrap(), 17);
    assert_eq!(second.property(Property::CalibratorState).await.unwrap(), 3);
    second.calibrator_off().await.unwrap();
    assert_eq!(second.property(Property::CalibratorState).await.unwrap(), 1);
    assert_eq!(second.property(Property::Brightness).await.unwrap(), 0);
    drop(second);
    source.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_panel_runtime_uses_one_worker_for_two_outputs_and_cached_independent_status() {
    native_panel_runtime(false).await;
}
#[tokio::test]
async fn nested_native_panel_preserves_explicit_simulation_shared_light_cover_and_leases() {
    native_panel_runtime(true).await;
}
async fn native_panel_runtime(nested: bool) {
    use regain_hub::{
        config::{DeviceType, HubConfig, OutputConfig, VirtualDevice},
        covercalibrator::CoverCalibratorProperty as Property,
        diagnostics::{Diagnostics, Reading},
        factory::NoCredentials,
        runtime::HubRuntime,
    };
    let Some(native) = runtime() else {
        return;
    };
    let source_config = config(NativeDevice::Ofp2, "SIM-OFP2");
    let mut config = HubConfig::empty();
    config.sources.push(source_config.clone());
    for number in [4, 17] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Simulated panel {number}"),
            device: VirtualDevice::Proxy {
                source: source_config.id,
                device_type: DeviceType::CoverCalibrator,
            },
        });
    }
    if nested {
        let mut previous = config.outputs[0].id;
        for number in [42, 91] {
            let source = Uuid::new_v4();
            config.sources.push(SourceConfig {
                id: source,
                label: "Nested native panel input".into(),
                polling: source_config.polling.clone(),
                backend: SourceBackend::Virtual { output: previous },
            });
            previous = Uuid::new_v4();
            config.outputs.push(OutputConfig {
                id: previous,
                number,
                label: "Nested native panel output".into(),
                device: VirtualDevice::Proxy {
                    source,
                    device_type: DeviceType::CoverCalibrator,
                },
            });
        }
    }
    let selected = if nested {
        config.outputs.last().unwrap().id
    } else {
        config.outputs[1].id
    };
    let selected_source = if nested {
        config.sources.last().unwrap().id
    } else {
        source_config.id
    };
    let runtime = HubRuntime::build(
        config.clone(),
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let snapshot = || runtime.source_snapshot(source_config.id).unwrap();
    assert_eq!(snapshot().lease_count, 0);
    let first = runtime.client();
    let second = runtime.client();
    first.connect(config.outputs[0].id).await.unwrap();
    second.connect(selected).await.unwrap();
    assert_eq!(snapshot().lease_count, 2);
    assert!(snapshot().simulated);
    first
        .connection(config.outputs[0].id)
        .unwrap()
        .covercalibrator()
        .unwrap()
        .calibrator_on(17)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while runtime
            .source_snapshot(selected_source)
            .unwrap()
            .values
            .get("brightness")
            != Some(&json!(17))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let status = runtime.output_status(selected, 0, 32).unwrap();
    assert!(status.simulated);
    let Diagnostics::CoverCalibrator { health, properties } = status.diagnostics else {
        panic!()
    };
    assert_eq!(health.source, selected_source);
    assert_eq!(properties.len(), 6);
    assert!(
        properties
            .iter()
            .all(|p| matches!(p.sample, Reading::Available { .. }))
    );
    first.close();
    tokio::time::timeout(Duration::from_secs(3), async {
        while snapshot().lease_count != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let connection = second.connection(selected).unwrap();
    assert_eq!(
        connection
            .covercalibrator()
            .unwrap()
            .property(Property::Brightness)
            .await
            .unwrap(),
        17
    );
    assert_eq!(
        connection
            .covercalibrator()
            .unwrap()
            .property(Property::CalibratorState)
            .await
            .unwrap(),
        3
    );
    if nested {
        let panel = connection.covercalibrator().unwrap();
        panel.calibrator_on(0).await.unwrap();
        assert_eq!(
            panel.property(Property::CalibratorState).await.unwrap(),
            json!(3)
        );
        assert_eq!(
            panel.property(Property::Brightness).await.unwrap(),
            json!(0)
        );
        panel.open_cover().await.unwrap();
        panel.halt_cover().await.unwrap();
        assert_eq!(
            panel.property(Property::CoverMoving).await.unwrap(),
            json!(false)
        );
        assert!(runtime.source_snapshots().iter().all(|s| s.simulated));
    }
    drop(connection);
    second.close();
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_efw_controller_uses_saved_or_standard_metadata_and_independent_worker_leases() {
    use regain_hub::filterwheel::{
        FilterWheelController, FilterWheelProperty, NativeFilterWheelMetadata,
    };
    let Some(native) = runtime() else {
        return;
    };
    for custom in [false, true] {
        let mut cfg = config(NativeDevice::Efw, "0102030405060708");
        let metadata = NativeFilterWheelMetadata {
            names: vec!["L", "R", "G", "B", "Hα", "OIII", "SII"]
                .into_iter()
                .map(String::from)
                .collect(),
            focus_offsets: vec![0, -12, 17, 30, 45, 52, 67],
        };
        if custom {
            let SourceBackend::Native { filter_wheel, .. } = &mut cfg.backend else {
                unreachable!()
            };
            *filter_wheel = Some(metadata.clone());
        }
        let expected_names = if custom {
            json!(metadata.names)
        } else {
            json!(
                (1..=7)
                    .map(|slot| format!("Filter {slot}"))
                    .collect::<Vec<_>>()
            )
        };
        let expected_offsets = if custom {
            json!(metadata.focus_offsets)
        } else {
            json!(vec![0; 7])
        };
        let restored: SourceConfig =
            serde_json::from_value(serde_json::to_value(&cfg).unwrap()).unwrap();
        let backend = NativeAccessoryBackend::new(&restored, native.clone()).unwrap();
        let source = SourceHandle::spawn(
            cfg.id,
            Uuid::new_v4(),
            cfg.polling.clone(),
            Box::new(backend),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let controller =
            FilterWheelController::new(source.clone(), Duration::from_secs(10)).unwrap();
        assert_eq!(source.snapshot().lease_count, 0);
        let first = controller.connect().await.unwrap();
        let second = controller.connect().await.unwrap();
        assert!(source.snapshot().simulated);
        assert_eq!(source.snapshot().lease_count, 2);
        assert_eq!(
            first.property(FilterWheelProperty::Names).await.unwrap(),
            expected_names
        );
        assert_eq!(
            second
                .property(FilterWheelProperty::FocusOffsets)
                .await
                .unwrap(),
            expected_offsets
        );
        first.move_to(6).await.unwrap();
        // The production EFW protocol simulator applies ordinary moves
        // immediately. Its timed calibration separately proves moving -1.
        assert_eq!(second.position().await.unwrap(), 6);
        drop(first);
        tokio::time::timeout(Duration::from_secs(5), async {
            while second.position().await.unwrap() == -1 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(second.position().await.unwrap(), 6);
        assert_eq!(source.snapshot().lease_count, 1);
        assert_eq!(
            second.property(FilterWheelProperty::Names).await.unwrap(),
            expected_names
        );
        assert_eq!(
            second
                .property(FilterWheelProperty::FocusOffsets)
                .await
                .unwrap(),
            expected_offsets
        );
        drop(second);
        source.shutdown().await.unwrap();
        let mut fresh = NativeAccessoryBackend::new(&restored, native.clone()).unwrap();
        fresh.connect().await.unwrap();
        assert_eq!(
            fresh.read("names".into(), Values::new()).await.unwrap(),
            expected_names
        );
        assert_eq!(
            fresh
                .read("focusoffsets".into(), Values::new())
                .await
                .unwrap(),
            expected_offsets
        );
        fresh.disconnect().await.unwrap();
    }
}

#[tokio::test]
async fn native_efw_mismatched_saved_slots_remain_an_error_instead_of_defaulting_or_authorizing_move()
 {
    use regain_hub::filterwheel::NativeFilterWheelMetadata;
    let Some(native) = runtime() else {
        return;
    };
    let mut cfg = config(NativeDevice::Efw, "0102030405060708");
    let SourceBackend::Native { filter_wheel, .. } = &mut cfg.backend else {
        unreachable!()
    };
    *filter_wheel = Some(NativeFilterWheelMetadata {
        names: vec!["L".into()],
        focus_offsets: vec![0],
    });
    let before = serde_json::to_value(&cfg).unwrap();
    let mut backend = NativeAccessoryBackend::new(&cfg, native).unwrap();
    backend.connect().await.unwrap();
    for member in ["names", "focusoffsets", "position"] {
        assert_eq!(
            backend
                .read(member.into(), Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unavailable
        );
    }
    assert_eq!(
        backend
            .write(
                "position".into(),
                Values::from([("Position".into(), json!(0))])
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(serde_json::to_value(&cfg).unwrap(), before);
    backend.disconnect().await.unwrap();
}

#[test]
fn invalid_native_filter_metadata_cannot_launch_a_worker_or_open_another_class() {
    use regain_hub::filterwheel::NativeFilterWheelMetadata;
    let native = NativeRuntime {
        directory: PathBuf::from("missing-private-worker"),
        simulate: true,
        references: None,
    };
    let mut cfg = config(NativeDevice::Eaf, "PRIVATE");
    let SourceBackend::Native { filter_wheel, .. } = &mut cfg.backend else {
        unreachable!()
    };
    *filter_wheel = Some(NativeFilterWheelMetadata {
        names: vec!["L".into()],
        focus_offsets: vec![0],
    });
    assert!(
        matches!(NativeAccessoryBackend::new(&cfg, native.clone()), Err(error) if error.kind == ErrorKind::InvalidValue)
    );
    let SourceBackend::Native {
        device,
        filter_wheel,
        ..
    } = &mut cfg.backend
    else {
        unreachable!()
    };
    *device = NativeDevice::Efw;
    filter_wheel.as_mut().unwrap().focus_offsets = vec![10];
    assert!(
        matches!(NativeAccessoryBackend::new(&cfg, native), Err(error) if error.kind == ErrorKind::InvalidValue)
    );
}
