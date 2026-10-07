use super::*;
use regain_hub::covercalibrator::CoverCalibratorProperty as Property;

fn patch(value: serde_json::Value) -> SimulationUpdate {
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn panel_simulation_ipc_rejects_stale_and_invalid_updates_without_changing_saved_revision() {
    use regain_hub::ipc::{Limits, read_frame, serve_stream};
    use tokio::io::AsyncWriteExt;
    async fn rpc(
        stream: &mut tokio::io::DuplexStream,
        id: u64,
        command: serde_json::Value,
    ) -> serde_json::Value {
        let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(&bytes).await.unwrap();
        serde_json::from_slice(
            &read_frame(stream, Duration::from_secs(2))
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
    let cfg = config(DeviceType::CoverCalibrator);
    let source = cfg.sources[0].id;
    let revision = cfg.revision;
    let hub = build(cfg);
    let (mut stream, server) = tokio::io::duplex(65536);
    let serving = tokio::spawn(serve_stream(server, hub.clone(), Limits::default()));
    rpc(&mut stream, 1, json!({"op":"hello"})).await;
    let mut update = json!({"op":"updateSimulation","source":source,"expectedRevision":Uuid::new_v4(),
        "update":{"coverCalibrator":{"coverState":4}}});
    assert_eq!(
        rpc(&mut stream, 2, update.clone()).await["error"]["code"],
        "revisionConflict"
    );
    assert_eq!(
        hub.source_snapshot(source)
            .unwrap()
            .simulation
            .unwrap()
            .cover_calibrator
            .unwrap()
            .cover_state,
        1
    );
    update["expectedRevision"] = json!(revision);
    let accepted = rpc(&mut stream, 3, update.clone()).await;
    assert_eq!(accepted["result"]["configurationRevision"], json!(revision));
    assert_eq!(
        accepted["result"]["simulation"]["coverCalibrator"]["coverState"],
        4
    );
    update["update"] = json!({"coverCalibrator":{"coverState":3,"brightness":4097}});
    assert_eq!(
        rpc(&mut stream, 4, update).await["error"]["code"],
        "invalidValue"
    );
    let status = rpc(&mut stream, 5, json!({"op":"sourceStatus","source":source})).await;
    assert_eq!(
        status["result"]["simulation"]["coverCalibrator"]["coverState"],
        4
    );
    assert_eq!(
        status["result"]["simulation"]["coverCalibrator"]["brightness"],
        0
    );
    assert_eq!(hub.revision(), revision);
    eventually(|| hub.source_snapshot(source).unwrap().lease_count == 0).await;
    drop(stream);
    serving.await.unwrap().unwrap();
    hub.shutdown().await.unwrap();
}
fn state(backend: &SimulatedBackend) -> regain_hub::simulated::CoverCalibratorState {
    backend
        .simulation_status()
        .unwrap()
        .cover_calibrator
        .unwrap()
}
async fn command(backend: &mut SimulatedBackend, member: &str) {
    backend.write(member.into(), Values::new()).await.unwrap();
}
async fn on(backend: &mut SimulatedBackend, brightness: i32) {
    backend
        .write(
            "calibratoron".into(),
            Values::from([("Brightness".into(), json!(brightness))]),
        )
        .await
        .unwrap();
}

#[test]
fn panel_updates_are_atomic_strict_bounded_and_component_specific() {
    let mut backend = SimulatedBackend::new(DeviceType::CoverCalibrator, vec![]).unwrap();
    let before = serde_json::to_value(backend.simulation_status()).unwrap();
    for value in [
        json!({"brightness":-1}),
        json!({"maxBrightness":0}),
        json!({"coverState":6}),
        json!({"calibratorState":-1}),
        json!({"brightness":4097,"calibratorState":3}),
        json!({"brightness":1}),
        json!({"coverState":0,"coverMoving":true}),
        json!({"calibratorState":0,"calibratorChanging":true}),
        json!({"moveDurationSeconds":301}),
        json!({"lightDurationSeconds":-1}),
    ] {
        assert_eq!(
            backend
                .update_simulation(patch(
                    json!({"coverCalibrator":value,"fault":"stalledMotion"})
                ))
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
        json!({"brightness":"0"}),
        json!({"coverMoving":0}),
        json!({"coverState":1.5}),
        json!({"maxBrightness":2147483648i64}),
        json!({"extra":true}),
    ] {
        assert!(
            serde_json::from_value::<SimulationUpdate>(json!({"coverCalibrator":value})).is_err()
        );
    }
    for value in [
        json!({"filterWheel":{}}),
        json!({"safe":true}),
        json!({"fault":"invalidSafety"}),
    ] {
        assert!(backend.update_simulation(patch(value)).is_err());
    }
    let mut other = SimulatedBackend::new(DeviceType::FilterWheel, vec![]).unwrap();
    assert!(
        other
            .update_simulation(patch(json!({"coverCalibrator":{}})))
            .is_err()
    );
    backend.update_simulation(patch(json!({"coverCalibrator":{"brightness":2147483647,"maxBrightness":2147483647,"calibratorState":3}}))).unwrap();
    assert_eq!(state(&backend).brightness, i32::MAX);
}

#[tokio::test(start_paused = true)]
async fn panel_clocks_are_independent_and_disconnect_never_halts_or_darkens() {
    let mut backend = SimulatedBackend::new(DeviceType::CoverCalibrator, vec![]).unwrap();
    backend
        .update_simulation(patch(
            json!({"coverCalibrator":{"moveDurationSeconds":1,"lightDurationSeconds":2}}),
        ))
        .unwrap();
    backend.connect().await.unwrap();
    assert_eq!(
        backend
            .read("interfaceversion".into(), Values::new())
            .await
            .unwrap(),
        json!(2)
    );
    on(&mut backend, 0).await;
    command(&mut backend, "opencover").await;
    assert_eq!(state(&backend).cover_state, 2);
    assert!(state(&backend).cover_moving && state(&backend).calibrator_changing);
    assert_eq!(state(&backend).calibrator_state, 2);
    tokio::time::advance(Duration::from_millis(500)).await;
    command(&mut backend, "haltcover").await;
    assert_eq!(state(&backend).cover_state, 4);
    assert!(!state(&backend).cover_moving && state(&backend).calibrator_changing);
    backend.disconnect().await.unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(state(&backend).calibrator_state, 3);
    assert_eq!(state(&backend).brightness, 0);
    assert_eq!(state(&backend).cover_state, 4);
    backend.connect().await.unwrap();
    command(&mut backend, "closecover").await;
    command(&mut backend, "calibratoroff").await;
    assert!(state(&backend).cover_moving);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(state(&backend).cover_state, 1);
    assert_eq!(state(&backend).calibrator_state, 1);
    assert!(!state(&backend).cover_moving && !state(&backend).calibrator_changing);
}

#[tokio::test(start_paused = true)]
async fn sparse_panel_updates_replace_only_the_selected_component_operation() {
    let mut backend = SimulatedBackend::new(DeviceType::CoverCalibrator, vec![]).unwrap();
    backend.connect().await.unwrap();
    backend
        .update_simulation(patch(
            json!({"coverCalibrator":{"moveDurationSeconds":2,"lightDurationSeconds":3}}),
        ))
        .unwrap();
    command(&mut backend, "opencover").await;
    on(&mut backend, 17).await;
    backend.update_simulation(patch(json!({"coverCalibrator":{"coverState":4,"coverMoving":false,"lightDurationSeconds":100}}))).unwrap();
    tokio::time::advance(Duration::from_secs(4)).await;
    assert_eq!(state(&backend).cover_state, 4);
    assert_eq!(state(&backend).brightness, 17);
    assert_eq!(state(&backend).calibrator_state, 3);
    command(&mut backend, "opencover").await;
    on(&mut backend, 20).await;
    backend.update_simulation(patch(json!({"coverCalibrator":{"calibratorState":1,"brightness":0,"calibratorChanging":false}}))).unwrap();
    assert!(
        backend
            .update_simulation(patch(
                json!({"coverCalibrator":{"coverState":0,"coverMoving":true}})
            ))
            .is_err()
    );
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(state(&backend).cover_state, 3);
    assert_eq!(state(&backend).calibrator_state, 1);
    assert_eq!(state(&backend).brightness, 0);
}

#[tokio::test(start_paused = true)]
async fn panel_faults_preserve_stalled_unknown_endpoint_and_wrong_light_readback() {
    let mut backend = SimulatedBackend::new(DeviceType::CoverCalibrator, vec![]).unwrap();
    backend.connect().await.unwrap();
    backend
        .update_simulation(patch(json!({"fault":"stalledMotion"})))
        .unwrap();
    command(&mut backend, "opencover").await;
    on(&mut backend, 0).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(state(&backend).cover_moving && state(&backend).calibrator_changing);
    assert_eq!(
        backend
            .write("closecover".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    backend
        .update_simulation(patch(json!({"fault":"stoppedShort"})))
        .unwrap();
    assert_eq!(state(&backend).cover_state, 4);
    assert!(!state(&backend).cover_moving);
    assert_eq!(state(&backend).brightness, 1);
    assert_eq!(state(&backend).calibrator_state, 3);
    backend
        .update_simulation(patch(json!({"fault":"invalidMotion"})))
        .unwrap();
    for property in [Property::CoverMoving, Property::CalibratorChanging] {
        let value = backend
            .read(property.member().into(), Values::new())
            .await
            .unwrap();
        assert_eq!(
            property.decode(&value).unwrap_err().kind,
            ErrorKind::Unavailable
        );
    }
    backend
        .update_simulation(patch(json!({"fault":"readError"})))
        .unwrap();
    assert_eq!(
        backend
            .read("brightness".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
}

#[tokio::test(start_paused = true)]
async fn absent_panel_components_are_independent_and_strict_arguments_never_actuate() {
    let mut backend = SimulatedBackend::new(DeviceType::CoverCalibrator, vec![]).unwrap();
    backend.connect().await.unwrap();
    for args in [
        json!({}),
        json!({"Brightness":-1}),
        json!({"Brightness":4097}),
        json!({"Brightness":2147483648i64}),
        json!({"Brightness":1.5}),
        json!({"Brightness":"1"}),
        json!({"brightness":1}),
        json!({"Brightness":1,"extra":true}),
    ] {
        assert_eq!(
            backend
                .write("calibratoron".into(), serde_json::from_value(args).unwrap())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    for member in ["opencover", "closecover", "haltcover", "calibratoroff"] {
        assert_eq!(
            backend
                .write(member.into(), Values::from([("extra".into(), json!(true))]))
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(state(&backend).cover_state, 1);
    assert_eq!(state(&backend).calibrator_state, 1);
    backend
        .update_simulation(patch(json!({"coverCalibrator":{"calibratorState":0}})))
        .unwrap();
    assert_eq!(
        backend
            .write("calibratoroff".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    command(&mut backend, "opencover").await;
    command(&mut backend, "haltcover").await;
    backend
        .update_simulation(patch(
            json!({"coverCalibrator":{"coverState":0,"calibratorState":1}}),
        ))
        .unwrap();
    assert_eq!(
        backend
            .write("opencover".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    on(&mut backend, 0).await;
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(state(&backend).calibrator_state, 3);
}

#[tokio::test(start_paused = true)]
async fn simulated_panel_applied_uncertainty_fences_siblings_until_all_leases_are_released() {
    let cfg = config(DeviceType::CoverCalibrator);
    let source = cfg.sources[0].id;
    let output = cfg.outputs[0].id;
    let hub = build(cfg);
    let first = hub.client();
    let second = hub.client();
    first.connect(output).await.unwrap();
    second.connect(output).await.unwrap();
    let a = first.connection(output).unwrap();
    let b = second.connection(output).unwrap();
    hub.update_simulation(source, patch(json!({"fault":"uncertainWrite"})))
        .await
        .unwrap();
    assert_eq!(
        a.covercalibrator()
            .unwrap()
            .calibrator_on(17)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    hub.update_simulation(source, patch(json!({"fault":"none"})))
        .await
        .unwrap();
    for result in [
        b.covercalibrator().unwrap().calibrator_off().await,
        b.covercalibrator().unwrap().close_cover().await,
        b.covercalibrator().unwrap().halt_cover().await,
        b.covercalibrator().unwrap().open_cover().await,
        b.covercalibrator().unwrap().calibrator_on(18).await,
    ] {
        assert_eq!(result.unwrap_err().kind, ErrorKind::Uncertain);
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        hub.source_snapshot(source)
            .unwrap()
            .simulation
            .unwrap()
            .cover_calibrator
            .unwrap()
            .brightness,
        17
    );
    assert!(hub.source_snapshot(source).unwrap().write_uncertain);
    first.close();
    second.close();
    drop(a);
    drop(b);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    let fresh = hub.client();
    fresh.connect(output).await.unwrap();
    let connection = fresh.connection(output).unwrap();
    assert_eq!(
        connection
            .covercalibrator()
            .unwrap()
            .property(Property::Brightness)
            .await
            .unwrap(),
        json!(17)
    );
    connection
        .covercalibrator()
        .unwrap()
        .calibrator_off()
        .await
        .unwrap();
    fresh.close();
    drop(connection);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn simulated_panel_composes_shared_provenance_cache_age_and_independent_leases() {
    use regain_hub::config::{OutputConfig, SourceConfig, VirtualDevice};
    let mut cfg = config(DeviceType::CoverCalibrator);
    let source = cfg.sources[0].id;
    let leaf = cfg.outputs[0].id;
    let mut previous = leaf;
    for number in [42, 91] {
        let id = Uuid::new_v4();
        cfg.sources.push(SourceConfig {
            id,
            label: "Nested panel simulation".into(),
            backend: SourceBackend::Virtual { output: previous },
            polling: cfg.sources[0].polling.clone(),
        });
        previous = Uuid::new_v4();
        cfg.outputs.push(OutputConfig {
            id: previous,
            number,
            label: "Nested panel".into(),
            device: VirtualDevice::Proxy {
                source: id,
                device_type: DeviceType::CoverCalibrator,
            },
        });
    }
    let hub = build(cfg.clone());
    hub.update_simulation(source, patch(json!({"sampleAgeSeconds":7})))
        .await
        .unwrap();
    eventually(|| hub.source_snapshots().iter().all(|s| s.lease_count == 0)).await;
    assert!(
        hub.source_snapshots()
            .iter()
            .all(|s| s.simulated && s.lease_count == 0)
    );
    let first = hub.client();
    let second = hub.client();
    first.connect(leaf).await.unwrap();
    second.connect(previous).await.unwrap();
    let a = first.connection(leaf).unwrap();
    let b = second.connection(previous).unwrap();
    a.covercalibrator().unwrap().calibrator_on(0).await.unwrap();
    a.covercalibrator().unwrap().open_cover().await.unwrap();
    eventually(|| {
        hub.source_snapshot(cfg.sources[2].id)
            .unwrap()
            .sample_ages_seconds
            .get("brightness")
            .is_some_and(|age| *age >= 7.0)
    })
    .await;
    assert_eq!(
        b.covercalibrator()
            .unwrap()
            .property(Property::Brightness)
            .await
            .unwrap(),
        json!(0)
    );
    first.close();
    drop(a);
    eventually(|| hub.source_snapshot(source).unwrap().lease_count == 1).await;
    assert!(b.connected());
    eventually(|| {
        hub.source_snapshot(source)
            .unwrap()
            .simulation
            .unwrap()
            .cover_calibrator
            .unwrap()
            .cover_state
            == 3
    })
    .await;
    assert_eq!(
        b.covercalibrator()
            .unwrap()
            .property(Property::CalibratorState)
            .await
            .unwrap(),
        json!(3)
    );
    second.close();
    drop(b);
    hub.shutdown().await.unwrap();
    assert!(hub.source_snapshots().iter().all(|s| s.lease_count == 0));
}
