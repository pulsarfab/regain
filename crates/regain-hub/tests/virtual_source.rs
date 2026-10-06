use regain_hub::{
    config::{DeviceType, HubConfig, OutputConfig, SourceBackend, VirtualDevice, WeatherMetric},
    factory::{NoCredentials, build_sources},
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    simulated::{Fault, SimulationUpdate},
    source::ErrorKind,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

fn native() -> NativeRuntime {
    NativeRuntime {
        directory: "no-hardware-workers".into(),
        simulate: false,
    }
}
fn config() -> HubConfig {
    let mut config: HubConfig =
        serde_json::from_str(include_str!("../examples/simulated-observatory.json")).unwrap();
    for source in &mut config.sources {
        source.polling.poll_seconds = 0.1;
        source.polling.request_timeout_seconds = 0.2;
    }
    if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
        members[0].policy.confirmation_seconds = 0.1;
        members[0].policy.safe_readings_to_safe = 1;
        members[0].policy.return_to_safe_hold_seconds = 0.0;
    }
    config
}
fn layer(config: &mut HubConfig, upstream: usize) -> (Uuid, Uuid) {
    let base = &config.outputs[upstream];
    let source = Uuid::new_v4();
    let output = Uuid::new_v4();
    config.sources.push(
        serde_json::from_value(json!({
            "id":source,"label":"Local output input","backend":{"kind":"virtual","output":base.id},
            "polling":{"pollSeconds":0.1,"requestTimeoutSeconds":0.2}
        }))
        .unwrap(),
    );
    let mut new: OutputConfig = base.clone();
    new.id = output;
    new.number = config
        .outputs
        .iter()
        .filter(|o| o.device.device_type() == base.device.device_type())
        .count() as u32;
    new.label = "Composed output".into();
    match &mut new.device {
        VirtualDevice::Switch { channels } => {
            for channel in channels {
                channel.id = Uuid::new_v4();
                channel.readout = serde_json::from_value(
                    json!({"kind":"channel","source":source,"channel":channel.number}),
                )
                .unwrap();
            }
        }
        VirtualDevice::Safety { members } => {
            for member in members {
                member.source = source;
            }
        }
        VirtualDevice::Weather { measurements } => {
            for (metric, measurement) in measurements {
                measurement.sources = vec![
                    serde_json::from_value(
                        json!({"kind":"property","source":source,"property":metric}),
                    )
                    .unwrap(),
                ];
            }
        }
        _ => unreachable!(),
    }
    config.outputs.push(new);
    (source, output)
}
fn build(config: HubConfig) -> Arc<HubRuntime> {
    HubRuntime::build(
        config,
        &native(),
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap()
}
async fn eventually(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn nested_switches_share_real_channels_permissions_and_release_all_internal_clients() {
    let mut config = config();
    let leaf = config.sources[0].id;
    let base = config.outputs[0].id;
    let (middle_source, _) = layer(&mut config, 0);
    let (outer_source, output) = layer(&mut config, 3);
    let hub = build(config);
    assert!(hub.outputs().iter().all(|o| o.simulated));
    let direct = hub.client();
    direct.connect(base).await.unwrap();
    let client = hub.client();
    client.connect(output).await.unwrap();
    assert_eq!(hub.source_snapshot(leaf).unwrap().lease_count, 2);
    let connection = client.connection(output).unwrap();
    connection
        .switch()
        .unwrap()
        .set_value(1, 37.4)
        .await
        .unwrap();
    eventually(|| connection.switch().unwrap().value(1).ok() == Some(37.0)).await;
    assert_eq!(
        direct
            .connection(base)
            .unwrap()
            .switch()
            .unwrap()
            .value(1)
            .unwrap(),
        37.0
    );
    assert!(!connection.switch().unwrap().can_write(2).await.unwrap());
    assert_eq!(
        connection
            .switch()
            .unwrap()
            .set_value(2, 20.0)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    let report = hub.inspect_source(outer_source, 0, 3).await.unwrap();
    assert_eq!(report.device_type, DeviceType::Switch);
    assert_eq!(report.simulation, Some(true));
    drop(connection);
    client.close();
    eventually(|| {
        hub.source_snapshot(middle_source).unwrap().lease_count == 0
            && hub.source_snapshot(outer_source).unwrap().lease_count == 0
    })
    .await;
    assert!(
        direct
            .connection(base)
            .unwrap()
            .switch()
            .unwrap()
            .value(1)
            .is_ok()
    );
    direct.close();
    eventually(|| {
        hub.active_connections() == 0 && hub.source_snapshots().iter().all(|s| s.lease_count == 0)
    })
    .await;
    let weak = Arc::downgrade(&hub);
    drop(direct);
    drop(client);
    drop(hub);
    eventually(|| weak.upgrade().is_none()).await;
}

#[tokio::test]
async fn nested_weather_preserves_sensor_age_and_refresh_reaches_the_leaf() {
    let mut config = config();
    let leaf = config.sources[2].id;
    let (_, middle) = layer(&mut config, 2);
    let (virtual_source, output) = layer(&mut config, 3);
    let hub = build(config);
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    eventually(|| {
        connection
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .is_ok()
    })
    .await;
    hub.update_simulation(
        leaf,
        SimulationUpdate {
            sample_age_seconds: Some(40.0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| {
        connection
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .is_ok_and(|r| r.age_seconds >= 40.0)
    })
    .await;
    let age = connection
        .weather()
        .unwrap()
        .time_since_last_update("temperature")
        .unwrap();
    assert!((40.0..41.0).contains(&age));
    connection.weather().unwrap().refresh().await.unwrap();
    eventually(|| {
        connection
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .is_ok_and(|r| r.age_seconds < 1.0)
    })
    .await;
    hub.update_simulation(
        leaf,
        SimulationUpdate {
            sample_age_seconds: Some(100.0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| {
        connection
            .weather()
            .unwrap()
            .read(WeatherMetric::Temperature)
            .is_err()
    })
    .await;
    let metadata =
        serde_json::to_value(hub.inspect_source(virtual_source, 0, 4).await.unwrap()).unwrap();
    let temperature = metadata["capabilities"]["measurements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["property"] == "temperature")
        .unwrap();
    assert_eq!(temperature["description"]["state"], "observed");
    assert_eq!(temperature["reading"]["state"], "unavailable");
    assert_eq!(temperature["ageSeconds"]["state"], "observed");
    assert!(
        hub.outputs()
            .iter()
            .find(|o| o.id == middle)
            .unwrap()
            .simulated
    );
    drop(connection);
    client.close();
    hub.shutdown().await.unwrap();
    assert_eq!(hub.active_connections(), 0);
}

#[tokio::test]
async fn nested_safety_cannot_count_cached_safe_as_new_confirmation_or_refresh_its_age() {
    let mut config = config();
    let leaf = config.sources[1].id;
    config.sources[1].polling.poll_seconds = 10.0;
    if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
        members[0].policy.confirmation_seconds = 10.0;
    }
    let (_, output) = layer(&mut config, 1);
    if let VirtualDevice::Safety { members } = &mut config.outputs[3].device {
        members[0].policy.confirmation_seconds = 0.1;
        members[0].policy.safe_readings_to_safe = 3;
    }
    let hub = build(config);
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    eventually(|| {
        connection
            .safety()
            .unwrap()
            .snapshot()
            .endpoints
            .values()
            .all(|s| s.last_sequence > 0)
    })
    .await;
    hub.update_simulation(
        leaf,
        SimulationUpdate {
            safe: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let state = connection.safety().unwrap().snapshot();
    assert!(!state.is_safe);
    assert!(state.endpoints.values().all(|e| e.safe_readings <= 1));
    assert!(
        state
            .endpoints
            .values()
            .any(|e| e.safe_age_seconds.is_some_and(|age| age > 0.3))
    );
    drop(connection);
    client.close();
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn repeated_virtual_polls_cannot_extend_an_established_safe_deadline() {
    let mut config = config();
    let leaf = config.sources[1].id;
    let base = config.outputs[1].id;
    config.sources[1].polling.poll_seconds = 10.0;
    if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
        members[0].policy.confirmation_seconds = 10.0;
    }
    let (_, output) = layer(&mut config, 1);
    if let VirtualDevice::Safety { members } = &mut config.outputs[3].device {
        members[0].policy.confirmation_seconds = 0.1;
        members[0].policy.maximum_safe_age_seconds = 0.5;
    }
    let hub = build(config);
    let direct = hub.client();
    direct.connect(base).await.unwrap();
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    eventually(|| {
        connection
            .safety()
            .unwrap()
            .snapshot()
            .endpoints
            .values()
            .all(|s| s.last_sequence > 0)
    })
    .await;
    hub.update_simulation(
        leaf,
        SimulationUpdate {
            safe: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| connection.safety().unwrap().snapshot().is_safe).await;
    eventually(|| !connection.safety().unwrap().snapshot().is_safe).await;
    assert!(
        direct
            .connection(base)
            .unwrap()
            .safety()
            .unwrap()
            .snapshot()
            .is_safe,
        "The outer deadline expires while the inner policy still permits the same evidence"
    );
    drop(connection);
    client.close();
    direct.close();
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn connecting_a_new_safety_output_cannot_seed_from_an_already_safe_inner_output() {
    let mut config = config();
    let leaf = config.sources[1].id;
    let base = config.outputs[1].id;
    config.sources[1].polling.poll_seconds = 10.0;
    if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
        members[0].policy.confirmation_seconds = 10.0;
    }
    let (_, output) = layer(&mut config, 1);
    if let VirtualDevice::Safety { members } = &mut config.outputs[3].device {
        members[0].policy.confirmation_seconds = 0.1;
    }
    let hub = build(config);
    let direct = hub.client();
    direct.connect(base).await.unwrap();
    hub.update_simulation(
        leaf,
        SimulationUpdate {
            safe: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| {
        direct
            .connection(base)
            .unwrap()
            .safety()
            .unwrap()
            .snapshot()
            .is_safe
    })
    .await;
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!connection.safety().unwrap().snapshot().is_safe);
    hub.update_simulation(
        leaf,
        SimulationUpdate {
            safe: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    eventually(|| connection.safety().unwrap().snapshot().is_safe).await;
    drop(connection);
    client.close();
    direct.close();
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn uncertain_nested_switch_write_is_not_replayed_and_shutdown_breaks_active_graph() {
    let mut config = config();
    let leaf = config.sources[0].id;
    let base = config.outputs[0].id;
    let (virtual_source, output) = layer(&mut config, 0);
    let hub = build(config);
    // A separate direct client keeps the leaf session alive while the virtual
    // transport retires its own client after an ambiguous command.
    let direct = hub.client();
    direct.connect(base).await.unwrap();
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    hub.update_simulation(
        leaf,
        SimulationUpdate {
            fault: Some(Fault::UncertainWrite),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        connection
            .switch()
            .unwrap()
            .set_value(1, 55.0)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert!(hub.source_snapshot(leaf).unwrap().write_uncertain);
    assert!(hub.source_snapshot(virtual_source).unwrap().write_uncertain);
    assert_eq!(
        direct
            .connection(base)
            .unwrap()
            .switch()
            .unwrap()
            .set_value(1, 60.0)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        hub.source_snapshot(leaf)
            .unwrap()
            .simulation
            .unwrap()
            .switch_values[&1],
        55.0
    );
    assert_eq!(
        connection
            .switch()
            .unwrap()
            .set_value(1, 60.0)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    hub.shutdown().await.unwrap();
    assert!(client.connection(output).is_err());
    drop(connection);
    drop(client);
    drop(direct);
    let weak = Arc::downgrade(&hub);
    drop(hub);
    eventually(|| weak.upgrade().is_none()).await;
}

#[tokio::test]
async fn inner_grace_is_not_promoted_to_fresh_safe_evidence_by_virtual_polling() {
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let failing = Arc::new(AtomicBool::new(false));
    let fail = failing.clone();
    let app = axum::Router::new().route(
        "/api/v1/safetymonitor/0/{member}",
        axum::routing::get(
            move |axum::extract::Path(member): axum::extract::Path<String>| {
                let fail = fail.clone();
                async move {
                    if member == "issafe" && fail.load(SeqCst) {
                        return (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            axum::Json(json!({})),
                        );
                    }
                    let value = match member.as_str() {
                        "interfaceversion" => json!(3),
                        "connected" | "issafe" => json!(true),
                        _ => panic!("Unexpected {member}"),
                    };
                    (
                        axum::http::StatusCode::OK,
                        axum::Json(json!({"ErrorNumber":0,"Value":value})),
                    )
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = config();
    let base = config.outputs[1].id;
    config.sources[1].backend = serde_json::from_value(json!({"kind":"alpaca","baseUrl":format!("http://{address}/"),"deviceType":"safetymonitor","deviceNumber":0})).unwrap();
    config.sources[1].polling.initial_backoff_seconds = 0.1;
    config.sources[1].polling.backoff_cap_seconds = 0.1;
    if let VirtualDevice::Safety { members } = &mut config.outputs[1].device {
        members[0].policy.failed_cycles_to_unsafe = 1000;
    }
    let (_, output) = layer(&mut config, 1);
    let hub = build(config);
    let direct = hub.client();
    direct.connect(base).await.unwrap();
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    eventually(|| connection.safety().unwrap().snapshot().is_safe).await;
    failing.store(true, SeqCst);
    eventually(|| {
        connection
            .safety()
            .unwrap()
            .snapshot()
            .endpoints
            .values()
            .any(|e| e.phase == regain_hub::safety::Phase::GraceSafe)
    })
    .await;
    let inner = direct
        .connection(base)
        .unwrap()
        .safety()
        .unwrap()
        .snapshot();
    assert!(inner.is_safe);
    assert!(
        inner
            .endpoints
            .values()
            .any(|e| e.phase == regain_hub::safety::Phase::GraceSafe)
    );
    assert!(
        connection
            .safety()
            .unwrap()
            .snapshot()
            .endpoints
            .values()
            .all(|e| !e.recovery_confirmed)
    );
    failing.store(false, SeqCst);
    eventually(|| {
        connection
            .safety()
            .unwrap()
            .snapshot()
            .endpoints
            .values()
            .all(|e| e.recovery_confirmed)
    })
    .await;
    drop(connection);
    client.close();
    direct.close();
    hub.shutdown().await.unwrap();
    server.abort();
}

#[tokio::test]
async fn cancelled_nested_connection_releases_pending_internal_clients_after_source_deadline() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = requested.clone();
    let app = axum::Router::new().fallback(axum::routing::get(move || {
        let seen = seen.clone();
        async move {
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
            std::future::pending::<axum::Json<serde_json::Value>>().await
        }
    }));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let mut config = config();
    config.sources[0].backend = serde_json::from_value(json!({"kind":"alpaca","baseUrl":format!("http://{address}/"),"deviceType":"switch","deviceNumber":0})).unwrap();
    layer(&mut config, 0);
    let (_, output) = layer(&mut config, 3);
    let hub = build(config);
    let client = hub.client();
    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.connect(output).await }
    });
    eventually(|| requested.load(std::sync::atomic::Ordering::SeqCst)).await;
    pending.abort();
    let _ = pending.await;
    client.close();
    drop(client);
    eventually(|| {
        hub.active_connections() == 0 && hub.source_snapshots().iter().all(|s| s.lease_count == 0)
    })
    .await;
    hub.shutdown().await.unwrap();
    let weak = Arc::downgrade(&hub);
    drop(hub);
    eventually(|| weak.upgrade().is_none()).await;
    server.abort();
}

#[tokio::test]
async fn graph_validation_and_unbound_factories_fail_before_starting_any_source() {
    let mut config = config();
    let (source, output) = layer(&mut config, 0);
    assert!(
        build_sources(
            &config,
            &native(),
            &NoCredentials,
            Arc::new(MonotonicClock::default())
        )
        .is_err()
    );
    config
        .sources
        .iter_mut()
        .find(|s| s.id == source)
        .unwrap()
        .backend = SourceBackend::Virtual { output };
    assert!(config.validate().iter().any(|e| e.code == "cycle"));
    assert!(
        HubRuntime::build(
            config,
            &native(),
            &NoCredentials,
            Arc::new(MonotonicClock::default())
        )
        .is_err()
    );
}
