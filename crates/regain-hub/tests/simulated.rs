use regain_hub::{
    config::{DeviceType, HubConfig, SourceBackend},
    factory::NoCredentials,
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    simulated::{Fault, SimulatedBackend, SimulationUpdate},
    source::{Backend, ErrorKind, Values},
};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use uuid::Uuid;

#[test]
fn setup_descriptors_share_backend_defaults_ranges_and_fault_choices() {
    let description = regain_hub::simulated::description();
    for kind in [
        DeviceType::Switch,
        DeviceType::SafetyMonitor,
        DeviceType::ObservingConditions,
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
fn build(config: HubConfig) -> Arc<HubRuntime> {
    HubRuntime::build(
        config,
        &NativeRuntime {
            directory: "missing-and-unused-worker-directory".into(),
            simulate: false,
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
