use regain_hub::{
    config::{DeviceType, HubConfig, SourceBackend},
    factory::NoCredentials,
    native::NativeRuntime,
    rotator::RotatorProperty,
    runtime::HubRuntime,
    safety::MonotonicClock,
    simulated::{Fault, FocuserUpdate, RotatorUpdate, SimulatedBackend, SimulationUpdate},
    source::{Backend, ErrorKind, Values},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

#[test]
fn rotator_state_updates_are_atomic_strict_and_class_specific() {
    let mut backend = SimulatedBackend::new(DeviceType::Rotator, vec![]).unwrap();
    let before = serde_json::to_value(backend.simulation_status()).unwrap();
    for rotator in [
        RotatorUpdate {
            position: Some(360.0),
            ..Default::default()
        },
        RotatorUpdate {
            mechanical_position: Some(-1.0),
            ..Default::default()
        },
        RotatorUpdate {
            target_position: Some(359.9999999),
            ..Default::default()
        },
        RotatorUpdate {
            step_size: Some(f64::MIN_POSITIVE),
            ..Default::default()
        },
        RotatorUpdate {
            step_size: Some(f64::MAX),
            ..Default::default()
        },
        RotatorUpdate {
            position: Some(20.0),
            move_duration_seconds: Some(301.0),
            ..Default::default()
        },
        RotatorUpdate {
            can_reverse: Some(false),
            reverse: Some(true),
            ..Default::default()
        },
        RotatorUpdate {
            position: Some(f64::NAN),
            ..Default::default()
        },
    ] {
        assert_eq!(
            backend
                .update_simulation(SimulationUpdate {
                    rotator: Some(rotator),
                    fault: Some(Fault::StoppedShort),
                    ..Default::default()
                })
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            serde_json::to_value(backend.simulation_status()).unwrap(),
            before
        );
    }
    for value in [
        json!({"rotator":{"position":"20"}}),
        json!({"rotator":{"isMoving":0}}),
        json!({"rotator":{"extra":true}}),
    ] {
        assert!(serde_json::from_value::<SimulationUpdate>(value).is_err());
    }
    let mut focuser = SimulatedBackend::new(DeviceType::Focuser, vec![]).unwrap();
    assert_eq!(
        focuser
            .update_simulation(SimulationUpdate {
                rotator: Some(RotatorUpdate::default()),
                ..Default::default()
            })
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(
        backend
            .update_simulation(SimulationUpdate {
                focuser: Some(FocuserUpdate::default()),
                ..Default::default()
            })
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
}

#[tokio::test(start_paused = true)]
async fn simulated_rotator_keeps_sync_offset_timed_commands_and_motion_after_disconnect() {
    let mut backend = SimulatedBackend::new(DeviceType::Rotator, vec![]).unwrap();
    backend.connect().await.unwrap();
    for (member, args) in [
        ("move", json!({"Position":"1"})),
        ("move", json!({"Position":1e39})),
        ("moveabsolute", json!({"Position":359.9999999})),
        ("movemechanical", json!({"Position":-1e-50})),
        ("sync", json!({"Position":360})),
        ("sync", json!({"position":10})),
        ("move", json!({"Position":1,"extra":true})),
        ("reverse", json!({"Reverse":1})),
        ("halt", json!({"extra":true})),
    ] {
        assert_eq!(
            backend
                .write(member.into(), serde_json::from_value(args).unwrap())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    backend
        .write(
            "sync".into(),
            Values::from([("Position".into(), json!(20))]),
        )
        .await
        .unwrap();
    assert_eq!(
        backend
            .read("mechanicalposition".into(), Values::new())
            .await
            .unwrap()
            .as_f64(),
        Some(0.0)
    );
    backend
        .write(
            "move".into(),
            Values::from([("Position".into(), json!(-721.5))]),
        )
        .await
        .unwrap();
    let state = backend.simulation_status().unwrap().rotator.unwrap();
    assert!(state.is_moving);
    assert_eq!(state.position, 20.0);
    assert_eq!(state.target_position, 18.5);
    backend.disconnect().await.unwrap();
    tokio::time::advance(Duration::from_millis(250)).await;
    let state = backend.simulation_status().unwrap().rotator.unwrap();
    assert!(!state.is_moving);
    assert_eq!(state.position, 18.5);
    assert_eq!(state.mechanical_position, 358.5);
    backend.connect().await.unwrap();
    backend
        .write(
            "sync".into(),
            Values::from([("Position".into(), json!(40))]),
        )
        .await
        .unwrap();
    backend
        .write(
            "moveabsolute".into(),
            Values::from([("Position".into(), json!(50))]),
        )
        .await
        .unwrap();
    backend
        .update_simulation(SimulationUpdate {
            rotator: Some(RotatorUpdate {
                step_size_available: Some(false),
                move_duration_seconds: Some(2.0),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    tokio::time::advance(Duration::from_millis(250)).await;
    let state = backend.simulation_status().unwrap().rotator.unwrap();
    assert_eq!((state.position, state.mechanical_position), (50.0, 8.5));
    assert_eq!(
        backend
            .read("stepsize".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    backend
        .write(
            "movemechanical".into(),
            Values::from([("Position".into(), json!(355))]),
        )
        .await
        .unwrap();
    assert!(
        backend
            .simulation_status()
            .unwrap()
            .rotator
            .unwrap()
            .is_moving
    );
    backend.write("halt".into(), Values::new()).await.unwrap();
    let halted = backend.simulation_status().unwrap().rotator.unwrap();
    assert_eq!(
        (
            halted.position,
            halted.mechanical_position,
            halted.target_position
        ),
        (50.0, 8.5, 50.0)
    );
    backend
        .write(
            "reverse".into(),
            Values::from([("Reverse".into(), json!(true))]),
        )
        .await
        .unwrap();
    assert!(
        backend
            .simulation_status()
            .unwrap()
            .rotator
            .unwrap()
            .reverse
    );
    backend
        .write(
            "movemechanical".into(),
            Values::from([("Position".into(), json!(355))]),
        )
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(3)).await;
    let state = backend.simulation_status().unwrap().rotator.unwrap();
    assert_eq!(
        (
            state.position,
            state.mechanical_position,
            state.target_position
        ),
        (36.5, 355.0, 36.5)
    );
    backend
        .update_simulation(SimulationUpdate {
            rotator: Some(RotatorUpdate {
                is_moving: Some(false),
                position: Some(1.0),
                mechanical_position: Some(0.0),
                target_position: Some(1.0),
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    backend
        .write(
            "move".into(),
            Values::from([("Position".into(), json!(f32::MAX as f64))]),
        )
        .await
        .unwrap();
    assert!(
        backend
            .simulation_status()
            .unwrap()
            .rotator
            .unwrap()
            .target_position
            < 360.0
    );
}

#[tokio::test(start_paused = true)]
async fn shared_simulated_rotator_uncertainty_fences_every_command_after_fault_clear() {
    let cfg = config(DeviceType::Rotator);
    let source = cfg.sources[0].id;
    let output = cfg.outputs[0].id;
    let hub = build(cfg);
    let a = hub.client();
    let b = hub.client();
    a.connect(output).await.unwrap();
    b.connect(output).await.unwrap();
    let first = a.connection(output).unwrap();
    let second = b.connection(output).unwrap();
    assert_eq!(hub.source_snapshot(source).unwrap().lease_count, 2);
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::UncertainWrite),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        first
            .rotator()
            .unwrap()
            .move_relative(12.5)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let rotator = second.rotator().unwrap();
    for error in [
        rotator.move_relative(12.5).await.unwrap_err(),
        rotator.move_absolute(30.0).await.unwrap_err(),
        rotator.move_mechanical(30.0).await.unwrap_err(),
        rotator.sync(30.0).await.unwrap_err(),
        rotator.set_reverse(true).await.unwrap_err(),
        rotator.halt().await.unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Uncertain);
    }
    tokio::time::advance(Duration::from_millis(250)).await;
    let state = hub
        .update_simulation(source, SimulationUpdate::default())
        .await
        .unwrap()
        .rotator
        .unwrap();
    assert_eq!(
        (
            state.position,
            state.mechanical_position,
            state.target_position
        ),
        (12.5, 12.5, 12.5)
    );
    assert!(!state.is_moving);
    assert!(hub.source_snapshot(source).unwrap().write_uncertain);
    assert!(!rotator.connected());
    assert_eq!(
        rotator
            .property(RotatorProperty::Position)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Disconnected
    );
    a.close();
    b.close();
    drop(first);
    drop(second);
    hub.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn simulated_rotator_faults_and_optional_properties_remain_explicit() {
    let cfg = config(DeviceType::Rotator);
    let source = cfg.sources[0].id;
    let output = cfg.outputs[0].id;
    let hub = build(cfg);
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    let rotator = connection.rotator().unwrap();
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::StalledMotion),
            rotator: Some(RotatorUpdate {
                step_size_available: Some(false),
                halt_available: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        rotator
            .property(RotatorProperty::StepSize)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    rotator.move_relative(20.0).await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(rotator.is_moving().await.unwrap());
    assert_eq!(rotator.sync(10.0).await.unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(
        rotator.set_reverse(true).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        rotator.halt().await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    hub.update_simulation(
        source,
        SimulationUpdate {
            rotator: Some(RotatorUpdate {
                halt_available: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    rotator.halt().await.unwrap();
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::StoppedShort),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    rotator.move_absolute(0.5).await.unwrap();
    tokio::time::advance(Duration::from_millis(250)).await;
    assert!(!rotator.is_moving().await.unwrap());
    assert_eq!(
        rotator
            .property(RotatorProperty::Position)
            .await
            .unwrap()
            .as_f64(),
        Some(359.5)
    );
    assert_eq!(
        rotator
            .property(RotatorProperty::TargetPosition)
            .await
            .unwrap()
            .as_f64(),
        Some(0.5)
    );
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::InvalidMotion),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        rotator.is_moving().await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        rotator.move_relative(1.0).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        rotator
            .property(RotatorProperty::TargetPosition)
            .await
            .unwrap()
            .as_f64(),
        Some(0.5)
    );
    client.close();
    drop(connection);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn rotator_simulation_composes_nested_outputs_preserving_identity_age_and_no_implicit_leases()
{
    let mut cfg = config(DeviceType::Rotator);
    let source = cfg.sources[0].id;
    let revision = cfg.revision;
    let mut output = cfg.outputs[0].id;
    let mut outer_source = source;
    for number in [7, 19] {
        outer_source = Uuid::new_v4();
        cfg.sources.push(
            serde_json::from_value(
                json!({"id":outer_source,"label":"Virtual rotator simulator",
            "backend":{"kind":"virtual","output":output},"polling":{"pollSeconds":0.1}}),
            )
            .unwrap(),
        );
        output = Uuid::new_v4();
        cfg.outputs.push(
            serde_json::from_value(json!({"id":output,"number":number,"label":"Nested rotator",
            "device":{"kind":"proxy","source":outer_source,"deviceType":"rotator"}}))
            .unwrap(),
        );
    }
    let hub = build(cfg);
    hub.update_simulation(
        source,
        SimulationUpdate {
            sample_age_seconds: Some(70.0),
            rotator: Some(RotatorUpdate {
                position: Some(20.0),
                mechanical_position: Some(350.0),
                target_position: Some(20.0),
                step_size_available: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(hub.outputs().iter().all(|output| output.simulated));
    // The shared update takes a temporary simulated-source control lease;
    // its drop is processed by the actor after the acknowledged reply.
    eventually(|| {
        hub.source_snapshots()
            .iter()
            .all(|source| source.lease_count == 0)
    })
    .await;
    assert!(
        hub.source_snapshots()
            .iter()
            .all(|source| !source.transport_connected)
    );
    assert_eq!(hub.source_snapshot(source).unwrap().revision, revision);
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    let rotator = connection.rotator().unwrap();
    assert_eq!(
        rotator
            .property(RotatorProperty::Position)
            .await
            .unwrap()
            .as_f64(),
        Some(20.0)
    );
    assert_eq!(
        rotator
            .property(RotatorProperty::MechanicalPosition)
            .await
            .unwrap()
            .as_f64(),
        Some(350.0)
    );
    assert_eq!(
        rotator
            .property(RotatorProperty::StepSize)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    rotator.move_relative(12.5).await.unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!rotator.is_moving().await.unwrap());
    assert_eq!(
        rotator
            .property(RotatorProperty::Position)
            .await
            .unwrap()
            .as_f64(),
        Some(32.5)
    );
    assert_eq!(
        rotator
            .property(RotatorProperty::MechanicalPosition)
            .await
            .unwrap()
            .as_f64(),
        Some(2.5)
    );
    eventually(|| {
        hub.source_snapshot(outer_source)
            .unwrap()
            .sample_ages_seconds
            .get("position")
            .is_some_and(|age| *age >= 70.0)
    })
    .await;
    client.close();
    drop(connection);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn simulated_modern_rotator_rejects_missing_required_reversal_without_dispatch() {
    let cfg = config(DeviceType::Rotator);
    let source = cfg.sources[0].id;
    let output = cfg.outputs[0].id;
    let hub = build(cfg);
    hub.update_simulation(
        source,
        SimulationUpdate {
            rotator: Some(RotatorUpdate {
                can_reverse: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = hub.client();
    assert_eq!(
        client.connect(output).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert!(client.connection(output).is_err());
    let state = hub
        .update_simulation(source, SimulationUpdate::default())
        .await
        .unwrap()
        .rotator
        .unwrap();
    assert!(!state.is_moving);
    assert_eq!(state.position, 0.0);
    client.close();
    hub.shutdown().await.unwrap();
}

#[test]
fn setup_descriptors_share_backend_defaults_ranges_and_fault_choices() {
    let description = regain_hub::simulated::description();
    for kind in [
        DeviceType::Switch,
        DeviceType::SafetyMonitor,
        DeviceType::ObservingConditions,
        DeviceType::Focuser,
        DeviceType::Rotator,
        DeviceType::FilterWheel,
        DeviceType::CoverCalibrator,
    ] {
        let state = serde_json::to_value(
            SimulatedBackend::new(kind, vec![])
                .unwrap()
                .simulation_status()
                .unwrap(),
        )
        .unwrap();
        let name = serde_json::to_value(kind).unwrap();
        let fields = description["controlsByDeviceType"][name.as_str().unwrap()]
            .as_array()
            .unwrap();
        for field in fields {
            let mut current = &state;
            for part in field["path"].as_array().unwrap() {
                current = &current[part.as_str().unwrap()];
            }
            assert_eq!(*current, field["default"]);
            if field["path"] == json!(["fault"]) {
                assert_eq!(
                    field["enum"],
                    description["faultsByDeviceType"][name.as_str().unwrap()]
                );
            }
            if field["path"][0] == "switchValues" {
                let id = field["path"][1].as_str().unwrap().parse::<u32>().unwrap();
                let mut backend = SimulatedBackend::new(kind, vec![]).unwrap();
                for key in ["minimum", "maximum"] {
                    backend
                        .update_simulation(SimulationUpdate {
                            switch_values: BTreeMap::from([(id, field[key].as_f64().unwrap())]),
                            ..Default::default()
                        })
                        .unwrap();
                }
            }
        }
    }
}
#[test]
fn tagged_simulation_commands_decode_canonical_channel_maps_and_reject_aliases() {
    let source = Uuid::new_v4();
    let revision = Uuid::new_v4();
    let command: regain_hub::ipc::Command = serde_json::from_value(json!({"op":"updateSimulation","source":source,"expectedRevision":revision,"update":{"switchValues":{"1":42.0}}})).unwrap();
    let regain_hub::ipc::Command::UpdateSimulation {
        expected_revision,
        update,
        ..
    } = command
    else {
        panic!("Wrong command");
    };
    assert_eq!(expected_revision, Some(revision));
    assert_eq!(update.switch_values[&1], 42.0);
    for map in [
        r#"{"01":2}"#,
        r#"{"1":2,"1":3}"#,
        r#"{"3":2}"#,
        r#"{"-1":2}"#,
    ] {
        let command = format!(
            r#"{{"op":"updateSimulation","source":"{source}","update":{{"switchValues":{map}}}}}"#
        );
        assert!(serde_json::from_str::<regain_hub::ipc::Command>(&command).is_err());
    }
}

fn config(kind: DeviceType) -> HubConfig {
    let mut config = HubConfig::empty();
    config.sources.push(
        serde_json::from_value(
            json!({"id":Uuid::new_v4(),"label":"Explicit simulation fixture",
        "backend":{"kind":"simulated","deviceType":kind}}),
        )
        .unwrap(),
    );
    config.sources[0].polling.poll_seconds = 0.1;
    config.sources[0].polling.request_timeout_seconds = 0.2;
    let source = config.sources[0].id;
    let device = match kind {
        DeviceType::SafetyMonitor => {
            json!({"kind":"safety","members":[{"source":source,"enabled":true,
            "policy":{"safeReadingsToSafe":1,"returnToSafeHoldSeconds":0.0,"confirmationSeconds":0.1}}]})
        }
        DeviceType::Switch => json!({"kind":"switch","channels":[{
            "id":Uuid::new_v4(),"number":0,"label":"Simulation level","readout":{"kind":"channel","source":source,"channel":1},
            "writable":true,"minimum":0.0,"maximum":100.0,"step":1.0,"units":"%"}]}),
        DeviceType::ObservingConditions => json!({"kind":"weather","measurements":{
            "temperature":{"sources":[{"kind":"property","source":source,"property":"temperature"}],"maximumAgeSeconds":1.0,"averageSeconds":0.0}
        }}),
        DeviceType::Focuser
        | DeviceType::Rotator
        | DeviceType::FilterWheel
        | DeviceType::CoverCalibrator => {
            json!({"kind":"proxy","source":source,"deviceType":kind})
        }
        _ => unreachable!(),
    };
    config.outputs.push(
        serde_json::from_value(
            json!({"id":Uuid::new_v4(),"number":0,"label":"Test output","device":device}),
        )
        .unwrap(),
    );
    assert!(config.validate().is_empty(), "{:?}", config.validate());
    config
}
#[path = "support/simulated_covercalibrator.rs"]
mod panel;
#[path = "support/simulated_filterwheel.rs"]
mod wheel;
fn build(config: HubConfig) -> Arc<HubRuntime> {
    HubRuntime::build(
        config,
        &NativeRuntime {
            cameras: None,
            directory: "missing-and-unused-worker-directory".into(),
            simulate: false,
            references: None,
        },
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap()
}
async fn eventually(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn safety_starts_unsafe_and_explicit_updates_use_the_real_shared_policy() {
    let config = config(DeviceType::SafetyMonitor);
    let source = config.sources[0].id;
    let output = config.outputs[0].id;
    let hub = build(config.clone());
    assert!(hub.outputs()[0].simulated);
    assert!(hub.source_snapshot(source).unwrap().simulated);
    let a = hub.client();
    let b = hub.client();
    a.connect(output).await.unwrap();
    b.connect(output).await.unwrap();
    let ca = a.connection(output).unwrap();
    let cb = b.connection(output).unwrap();
    assert!(!ca.safety().unwrap().snapshot().is_safe);
    hub.update_simulation(
        source,
        SimulationUpdate {
            safe: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| {
        ca.safety().unwrap().snapshot().is_safe && cb.safety().unwrap().snapshot().is_safe
    })
    .await;
    let report = hub.inspect_source(source, 0, 4).await.unwrap();
    assert_eq!(report.simulation, Some(true));
    hub.update_simulation(
        source,
        SimulationUpdate {
            safe: Some(false),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| {
        !ca.safety().unwrap().snapshot().is_safe && !cb.safety().unwrap().snapshot().is_safe
    })
    .await;
    drop(ca);
    drop(cb);
    a.close();
    b.close();
    hub.shutdown().await.unwrap();
    let fresh = build(config);
    assert!(
        !fresh
            .source_snapshot(source)
            .unwrap()
            .simulation
            .unwrap()
            .safe
    );
    fresh.shutdown().await.unwrap();
}
#[tokio::test]
async fn invalid_safety_and_read_timeouts_never_count_as_safe_observations() {
    for fault in [Fault::InvalidSafety, Fault::ReadError, Fault::Timeout] {
        let config = config(DeviceType::SafetyMonitor);
        let source = config.sources[0].id;
        let output = config.outputs[0].id;
        let hub = build(config);
        hub.update_simulation(
            source,
            SimulationUpdate {
                safe: Some(true),
                fault: Some(fault),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let client = hub.client();
        client.connect(output).await.unwrap();
        let connection = client.connection(output).unwrap();
        tokio::time::sleep(Duration::from_millis(240)).await;
        assert!(
            !connection.safety().unwrap().snapshot().is_safe,
            "{fault:?}"
        );
        hub.update_simulation(
            source,
            SimulationUpdate {
                fault: Some(Fault::None),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        eventually(|| connection.safety().unwrap().snapshot().is_safe).await;
        drop(connection);
        client.close();
        hub.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn switch_clients_share_values_and_uncertain_writes_are_not_replayed_or_cleared_by_test_controls()
 {
    let config = config(DeviceType::Switch);
    let source = config.sources[0].id;
    let output = config.outputs[0].id;
    let hub = build(config);
    let a = hub.client();
    let b = hub.client();
    a.connect(output).await.unwrap();
    b.connect(output).await.unwrap();
    let ca = a.connection(output).unwrap();
    let cb = b.connection(output).unwrap();
    ca.switch().unwrap().set_value(0, 24.6).await.unwrap();
    eventually(|| cb.switch().unwrap().value(0).ok() == Some(25.0)).await;
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::UncertainWrite),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        ca.switch()
            .unwrap()
            .set_value(0, 40.0)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        hub.source_snapshot(source)
            .unwrap()
            .simulation
            .unwrap()
            .switch_values[&1],
        40.0
    );
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(hub.source_snapshot(source).unwrap().write_uncertain);
    assert_eq!(
        cb.switch()
            .unwrap()
            .set_value(0, 50.0)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        hub.source_snapshot(source)
            .unwrap()
            .simulation
            .unwrap()
            .switch_values[&1],
        40.0
    );
    drop(ca);
    drop(cb);
    a.close();
    b.close();
    eventually(|| hub.source_snapshot(source).unwrap().lease_count == 0).await;
    let c = hub.client();
    c.connect(output).await.unwrap();
    c.connection(output)
        .unwrap()
        .switch()
        .unwrap()
        .set_value(0, 50.0)
        .await
        .unwrap();
    c.close();
    hub.shutdown().await.unwrap();
}
#[tokio::test]
async fn weather_uses_real_freshness_rules_and_absent_sensors_stay_unsupported() {
    use regain_hub::config::WeatherMetric as M;
    let config = config(DeviceType::ObservingConditions);
    let source = config.sources[0].id;
    let output = config.outputs[0].id;
    let hub = build(config);
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    eventually(|| connection.weather().unwrap().read(M::Temperature).is_ok()).await;
    hub.update_simulation(
        source,
        SimulationUpdate {
            sample_age_seconds: Some(10.0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| connection.weather().unwrap().read(M::Temperature).is_err()).await;
    connection.weather().unwrap().refresh().await.unwrap();
    eventually(|| connection.weather().unwrap().read(M::Temperature).is_ok()).await;
    hub.update_simulation(
        source,
        SimulationUpdate {
            weather: BTreeMap::from([(M::Temperature, None)]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| connection.weather().unwrap().read(M::Temperature).is_err()).await;
    let report = serde_json::to_value(hub.inspect_source(source, 0, 4).await.unwrap()).unwrap();
    let temperature = report["capabilities"]["measurements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["property"] == "temperature")
        .unwrap();
    assert_eq!(temperature["reading"]["state"], "unsupported");
    assert_eq!(temperature["ageSeconds"]["state"], "unsupported");
    drop(connection);
    client.close();
    hub.shutdown().await.unwrap();
}
#[test]
fn updates_are_atomic_class_specific_and_share_production_validation() {
    let mut switch = SimulatedBackend::new(DeviceType::Switch, vec![]).unwrap();
    let before = serde_json::to_value(switch.simulation_status()).unwrap();
    for update in [
        SimulationUpdate {
            switch_values: BTreeMap::from([(0, 1.0), (99, 2.0)]),
            ..Default::default()
        },
        SimulationUpdate {
            safe: Some(true),
            ..Default::default()
        },
        SimulationUpdate {
            sample_age_seconds: Some(f64::NAN),
            ..Default::default()
        },
        SimulationUpdate {
            fault: Some(Fault::InvalidSafety),
            ..Default::default()
        },
    ] {
        assert_eq!(
            switch.update_simulation(update).unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            serde_json::to_value(switch.simulation_status()).unwrap(),
            before
        );
    }
    assert!(SimulatedBackend::new(DeviceType::Camera, vec![]).is_err());
    let mut weather = SimulatedBackend::new(DeviceType::ObservingConditions, vec![]).unwrap();
    assert!(
        weather
            .update_simulation(SimulationUpdate {
                weather: BTreeMap::from([(
                    regain_hub::config::WeatherMetric::Humidity,
                    Some(101.0)
                )]),
                ..Default::default()
            })
            .is_err()
    );
}
#[tokio::test]
async fn ordinary_switch_commands_cannot_write_read_only_simulated_sensors() {
    let mut backend = SimulatedBackend::new(DeviceType::Switch, vec![]).unwrap();
    backend.connect().await.unwrap();
    let args = Values::from([("Id".into(), json!(2)), ("Value".into(), json!(20))]);
    assert_eq!(
        backend
            .write("setswitchvalue".into(), args)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    backend
        .update_simulation(SimulationUpdate {
            switch_values: BTreeMap::from([(2, 20.04)]),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        backend
            .read(
                "getswitchvalue".into(),
                Values::from([("Id".into(), json!(2))])
            )
            .await
            .unwrap(),
        json!(20.0)
    );
}
#[tokio::test]
async fn simulator_updates_reject_real_sources_before_opening_their_transport() {
    let mut config = HubConfig::empty();
    let source = Uuid::new_v4();
    config.sources.push(
        serde_json::from_value(json!({"id":source,"label":"No worker must start",
        "backend":{"kind":"native","device":"fc3","identity":"NOT-ATTACHED"}}))
        .unwrap(),
    );
    assert!(matches!(
        config.sources[0].backend,
        SourceBackend::Native { .. }
    ));
    let hub = build(config);
    assert_eq!(
        hub.update_simulation(source, SimulationUpdate::default())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(hub.source_snapshot(source).unwrap().lease_count, 0);
    assert!(hub.source_snapshot(source).unwrap().error.is_none());
    hub.shutdown().await.unwrap();
}

#[test]
fn focuser_state_patches_are_atomic_strict_and_class_specific() {
    let mut backend = SimulatedBackend::new(DeviceType::Focuser, vec![]).unwrap();
    let before = serde_json::to_value(backend.simulation_status()).unwrap();
    for focuser in [
        FocuserUpdate {
            max_step: Some(0),
            ..Default::default()
        },
        FocuserUpdate {
            max_step: Some(49999),
            ..Default::default()
        },
        FocuserUpdate {
            position: Some(-1),
            ..Default::default()
        },
        FocuserUpdate {
            temp_comp_available: Some(false),
            temp_comp: Some(true),
            ..Default::default()
        },
        FocuserUpdate {
            temperature: Some(f64::NAN),
            ..Default::default()
        },
        FocuserUpdate {
            step_size: Some(0.0),
            ..Default::default()
        },
        FocuserUpdate {
            move_duration_seconds: Some(301.0),
            ..Default::default()
        },
    ] {
        assert_eq!(
            backend
                .update_simulation(SimulationUpdate {
                    focuser: Some(focuser),
                    ..Default::default()
                })
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            serde_json::to_value(backend.simulation_status()).unwrap(),
            before
        );
    }
    assert!(
        backend
            .update_simulation(SimulationUpdate {
                fault: Some(Fault::InvalidSafety),
                ..Default::default()
            })
            .is_err()
    );
    for value in [
        json!({"focuser":{"position":1.5}}),
        json!({"focuser":{"position":2147483648i64}}),
        json!({"focuser":{"isMoving":"false"}}),
        json!({"focuser":{"unknown":1}}),
    ] {
        assert!(serde_json::from_value::<SimulationUpdate>(value).is_err());
    }
    let mut switch = SimulatedBackend::new(DeviceType::Switch, vec![]).unwrap();
    for update in [
        SimulationUpdate {
            focuser: Some(FocuserUpdate::default()),
            ..Default::default()
        },
        SimulationUpdate {
            fault: Some(Fault::InvalidMotion),
            ..Default::default()
        },
    ] {
        assert_eq!(
            switch.update_simulation(update).unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
}

#[tokio::test(start_paused = true)]
async fn simulated_move_acknowledges_start_and_completion_continues_after_disconnect() {
    let mut backend = SimulatedBackend::new(DeviceType::Focuser, vec![]).unwrap();
    backend.connect().await.unwrap();
    for args in [
        json!({"Position":1.5}),
        json!({"Position":"50100"}),
        json!({"Position":2147483648i64}),
        json!({"Position":50100,"extra":true}),
        json!({"Position":52000}),
    ] {
        assert_eq!(
            backend
                .write("move".into(), serde_json::from_value(args).unwrap())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    backend
        .write(
            "move".into(),
            Values::from([("Position".into(), json!(50100))]),
        )
        .await
        .unwrap();
    assert_eq!(
        backend
            .read("ismoving".into(), Values::new())
            .await
            .unwrap(),
        true
    );
    assert_eq!(
        backend
            .read("position".into(), Values::new())
            .await
            .unwrap(),
        50000
    );
    backend.disconnect().await.unwrap();
    assert!(
        backend
            .simulation_status()
            .unwrap()
            .focuser
            .unwrap()
            .is_moving
    );
    tokio::time::advance(Duration::from_millis(250)).await;
    let state = backend.simulation_status().unwrap().focuser.unwrap();
    assert!(!state.is_moving);
    assert_eq!(state.position, 50100);
    backend.connect().await.unwrap();
    assert_eq!(
        backend
            .read("position".into(), Values::new())
            .await
            .unwrap(),
        50100
    );
}

#[tokio::test(start_paused = true)]
async fn simulated_relative_focuser_preserves_optional_errors_and_signed_motion() {
    let config = config(DeviceType::Focuser);
    let source = config.sources[0].id;
    let output = config.outputs[0].id;
    let hub = build(config);
    hub.update_simulation(
        source,
        SimulationUpdate {
            focuser: Some(FocuserUpdate {
                absolute: Some(false),
                temperature_available: Some(false),
                step_size_available: Some(false),
                halt_available: Some(false),
                temp_comp_available: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    let focuser = connection.focuser().unwrap();
    assert!(!focuser.capabilities().await.unwrap().absolute);
    for error in [
        focuser.position().await.unwrap_err(),
        focuser.temperature().await.unwrap_err(),
        focuser.step_size().await.unwrap_err(),
        focuser.halt().await.unwrap_err(),
        focuser.set_temp_comp(true).await.unwrap_err(),
    ] {
        assert_eq!(error.kind, ErrorKind::Unsupported);
    }
    assert_eq!(
        focuser.move_to(i32::MIN).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    focuser.move_to(-30).await.unwrap();
    assert!(focuser.is_moving().await.unwrap());
    tokio::time::advance(Duration::from_millis(250)).await;
    assert!(!focuser.is_moving().await.unwrap());
    assert_eq!(
        focuser.position().await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    client.close();
    drop(connection);
    hub.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn shared_simulated_focuser_uncertainty_survives_fault_clear_without_replaying() {
    let config = config(DeviceType::Focuser);
    let source = config.sources[0].id;
    let output = config.outputs[0].id;
    let hub = build(config);
    let a = hub.client();
    let b = hub.client();
    a.connect(output).await.unwrap();
    b.connect(output).await.unwrap();
    let first = a.connection(output).unwrap();
    let second = b.connection(output).unwrap();
    assert_eq!(hub.source_snapshot(source).unwrap().lease_count, 2);
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::UncertainWrite),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        first
            .focuser()
            .unwrap()
            .move_to(50100)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        second
            .focuser()
            .unwrap()
            .move_to(50200)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::None),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(hub.source_snapshot(source).unwrap().write_uncertain);
    assert_eq!(
        second.focuser().unwrap().halt().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    tokio::time::advance(Duration::from_millis(250)).await;
    assert!(!second.focuser().unwrap().connected());
    assert_eq!(
        second.focuser().unwrap().position().await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    let state = hub
        .update_simulation(source, SimulationUpdate::default())
        .await
        .unwrap()
        .focuser
        .unwrap();
    assert_eq!(state.position, 50100);
    assert!(!state.is_moving);
    a.close();
    b.close();
    drop(first);
    drop(second);
    hub.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn simulated_motion_faults_cover_stalls_stopped_short_and_invalid_idle_readings() {
    let config = config(DeviceType::Focuser);
    let source = config.sources[0].id;
    let output = config.outputs[0].id;
    let hub = build(config);
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    let focuser = connection.focuser().unwrap();
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::StalledMotion),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    focuser.move_to(50100).await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(focuser.is_moving().await.unwrap());
    assert_eq!(focuser.position().await.unwrap(), 50000);
    assert_eq!(
        focuser.move_to(50200).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    focuser.halt().await.unwrap();
    assert!(!focuser.is_moving().await.unwrap());
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::StoppedShort),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    focuser.move_to(50200).await.unwrap();
    tokio::time::advance(Duration::from_millis(250)).await;
    assert!(!focuser.is_moving().await.unwrap());
    assert_eq!(focuser.position().await.unwrap(), 50199);
    hub.update_simulation(
        source,
        SimulationUpdate {
            fault: Some(Fault::InvalidMotion),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        focuser.is_moving().await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        focuser.move_to(50200).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(focuser.position().await.unwrap(), 50199);
    client.close();
    drop(connection);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn focuser_simulation_status_updates_without_leases_or_configuration_changes_and_composes() {
    let mut config = config(DeviceType::Focuser);
    let source = config.sources[0].id;
    let base = config.outputs[0].id;
    let virtual_source = Uuid::new_v4();
    let output = Uuid::new_v4();
    config.sources.push(
        serde_json::from_value(json!({"id":virtual_source,"label":"Virtual simulator",
        "backend":{"kind":"virtual","output":base},"polling":{"pollSeconds":0.1}}))
        .unwrap(),
    );
    config.outputs.push(
        serde_json::from_value(
            json!({"id":output,"number":4,"label":"Simulation focuser alias",
        "device":{"kind":"proxy","source":virtual_source,"deviceType":"focuser"}}),
        )
        .unwrap(),
    );
    let revision = config.revision;
    let hub = build(config);
    let update = hub
        .update_simulation(
            source,
            SimulationUpdate {
                sample_age_seconds: Some(70.0),
                focuser: Some(FocuserUpdate {
                    max_step: Some(i32::MAX),
                    position: Some(100000),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(update.focuser.unwrap().position, 100000);
    eventually(|| {
        hub.source_snapshots()
            .iter()
            .all(|source| source.lease_count == 0)
    })
    .await;
    assert_eq!(hub.source_snapshot(source).unwrap().revision, revision);
    assert!(hub.outputs().iter().all(|output| output.simulated));
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    assert_eq!(
        connection.focuser().unwrap().position().await.unwrap(),
        100000
    );
    assert_eq!(
        connection
            .focuser()
            .unwrap()
            .capabilities()
            .await
            .unwrap()
            .max_step,
        i32::MAX
    );
    eventually(|| {
        hub.source_snapshot(virtual_source)
            .unwrap()
            .sample_ages_seconds
            .get("position")
            .is_some_and(|age| *age >= 70.0)
    })
    .await;
    client.close();
    drop(connection);
    hub.shutdown().await.unwrap();
}
