use super::*;
use regain_hub::filterwheel::FilterWheelProperty;

fn patch(value: serde_json::Value) -> SimulationUpdate {
    serde_json::from_value(value).unwrap()
}

#[tokio::test]
async fn applied_wheel_update_can_exceed_reply_budget_without_replay_or_stream_loss() {
    use regain_hub::ipc::{Limits, MAX_FRAME_BYTES, read_frame, serve_stream};
    use tokio::io::AsyncWriteExt;
    async fn rpc(
        stream: &mut tokio::io::DuplexStream,
        id: u64,
        command: serde_json::Value,
    ) -> serde_json::Value {
        let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
        assert!(bytes.len() <= MAX_FRAME_BYTES);
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
    let cfg = config(DeviceType::FilterWheel);
    let source = cfg.sources[0].id;
    let revision = cfg.revision;
    let hub = build(cfg);
    let mut state =
        serde_json::to_value(hub.source_snapshot(source).unwrap().simulation.unwrap()).unwrap();
    state["filterWheel"]["names"] = json!([""]);
    state["filterWheel"]["focusOffsets"] = json!([0]);
    let reply = json!({"version":1,"id":2,"result":{"source":source,"configurationRevision":revision,"simulation":state}});
    let length = MAX_FRAME_BYTES - serde_json::to_vec(&reply).unwrap().len() + 1;
    let names = json!(["x".repeat(length)]);
    let (mut stream, server) = tokio::io::duplex(65536);
    let serving = tokio::spawn(serve_stream(server, hub.clone(), Limits::default()));
    rpc(&mut stream, 1, json!({"op":"hello"})).await;
    let oversized=rpc(&mut stream,2,json!({"op":"updateSimulation","source":source,"expectedRevision":revision,"update":{"filterWheel":{"names":names,"focusOffsets":[0]}}})).await;
    assert_eq!(oversized["error"]["code"], "responseTooLarge");
    assert_eq!(
        hub.source_snapshot(source)
            .unwrap()
            .simulation
            .unwrap()
            .filter_wheel
            .unwrap()
            .names[0]
            .len(),
        length
    );
    assert_eq!(
        rpc(&mut stream, 3, json!({"op":"sourceStatus","source":source})).await["error"]["code"],
        "responseTooLarge"
    );
    let repaired=rpc(&mut stream,4,json!({"op":"updateSimulation","source":source,"expectedRevision":revision,"update":{"filterWheel":{"names":["L"]}}})).await;
    assert_eq!(
        repaired["result"]["simulation"]["filterWheel"]["names"],
        json!(["L"])
    );
    assert_eq!(
        repaired["result"]["simulation"]["filterWheel"]["focusOffsets"],
        json!([0])
    );
    assert!(!hub.source_snapshot(source).unwrap().write_uncertain);
    eventually(|| hub.source_snapshot(source).unwrap().lease_count == 0).await;
    drop(stream);
    serving.await.unwrap().unwrap();
    hub.shutdown().await.unwrap();
}

#[test]
fn wheel_updates_are_atomic_bounded_and_class_specific() {
    let mut backend = SimulatedBackend::new(DeviceType::FilterWheel, vec![]).unwrap();
    let before = serde_json::to_value(backend.simulation_status()).unwrap();
    for value in [
        json!({"filterWheel":{"names":[]}}),
        json!({"filterWheel":{"names":["L"]}}),
        json!({"filterWheel":{"focusOffsets":[0]}}),
        json!({"filterWheel":{"focusOffsets":vec![1;7]}}),
        json!({"filterWheel":{"position":-2}}),
        json!({"filterWheel":{"position":7}}),
        json!({"filterWheel":{"position":1,"moveDurationSeconds":301}}),
        json!({"filterWheel":{"moveDurationSeconds":-1}}),
        json!({"filterWheel":{"names":vec!["";1025],"focusOffsets":vec![0;1025]}}),
        json!({"filterWheel":{"names":vec!["x".repeat(1024*1024+1);7]}}),
        json!({"rotator":{}}),
        json!({"safe":true}),
        json!({"fault":"invalidSafety"}),
    ] {
        let mut update = patch(value);
        if update.fault.is_none() {
            update.fault = Some(Fault::StoppedShort);
        }
        assert_eq!(
            backend.update_simulation(update).unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            serde_json::to_value(backend.simulation_status()).unwrap(),
            before
        );
    }
    for value in [
        json!({"filterWheel":{"position":"0"}}),
        json!({"filterWheel":{"position":1.5}}),
        json!({"filterWheel":{"focusOffsets":[0,2147483648i64]}}),
        json!({"filterWheel":{"names":[0]}}),
        json!({"filterWheel":{"focusOffsets":["0"]}}),
        json!({"filterWheel":{"halt":true}}),
    ] {
        assert!(serde_json::from_value::<SimulationUpdate>(value).is_err());
    }
    for kind in [DeviceType::Rotator, DeviceType::Focuser, DeviceType::Switch] {
        let mut other = SimulatedBackend::new(kind, vec![]).unwrap();
        assert_eq!(
            other
                .update_simulation(patch(json!({"filterWheel":{}})))
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    let result = backend
        .update_simulation(patch(json!({"filterWheel":{
            "names":["L","Hα",""],"focusOffsets":[-2147483648i64,0,2147483647],"position":2
        }})))
        .unwrap()
        .filter_wheel
        .unwrap();
    assert_eq!(result.names, vec!["L", "Hα", ""]);
    assert_eq!(result.focus_offsets, vec![i32::MIN, 0, i32::MAX]);
}

#[tokio::test(start_paused = true)]
async fn wheel_backend_preserves_actual_timed_motion_through_disconnect_and_sparse_updates() {
    let mut backend = SimulatedBackend::new(DeviceType::FilterWheel, vec![]).unwrap();
    backend.connect().await.unwrap();
    for value in [
        json!({"Position":"1"}),
        json!({"Position":1.5}),
        json!({"Position":-1}),
        json!({"Position":7}),
        json!({"Position":1,"extra":0}),
    ] {
        assert_eq!(
            backend
                .write("position".into(), serde_json::from_value(value).unwrap())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    for member in ["halt", "calibrate", "move", "commandstring"] {
        assert_eq!(
            backend
                .write(member.into(), Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
    }
    backend
        .write(
            "position".into(),
            Values::from([("Position".into(), json!(6))]),
        )
        .await
        .unwrap();
    assert_eq!(
        backend
            .read("position".into(), Values::new())
            .await
            .unwrap(),
        -1
    );
    assert_eq!(
        backend
            .write(
                "position".into(),
                Values::from([("Position".into(), json!(1))])
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    backend
        .update_simulation(patch(
            json!({"filterWheel":{"moveDurationSeconds":2},"sampleAgeSeconds":50}),
        ))
        .unwrap();
    backend.disconnect().await.unwrap();
    tokio::time::advance(Duration::from_millis(250)).await;
    assert_eq!(
        backend
            .simulation_status()
            .unwrap()
            .filter_wheel
            .unwrap()
            .position,
        6
    );
    backend.connect().await.unwrap();
    assert_eq!(
        backend
            .read("position".into(), Values::new())
            .await
            .unwrap(),
        6
    );
    backend
        .write(
            "position".into(),
            Values::from([("Position".into(), json!(0))]),
        )
        .await
        .unwrap();
    tokio::time::advance(Duration::from_millis(250)).await;
    assert_eq!(
        backend
            .read("position".into(), Values::new())
            .await
            .unwrap(),
        -1
    );
    backend
        .update_simulation(patch(json!({"filterWheel":{"position":3}})))
        .unwrap();
    tokio::time::advance(Duration::from_secs(3)).await;
    assert_eq!(
        backend
            .read("position".into(), Values::new())
            .await
            .unwrap(),
        3
    );
    backend.disconnect().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn wheel_faults_keep_stalled_stopped_short_and_invalid_position_explicit() {
    let cfg = config(DeviceType::FilterWheel);
    let source = cfg.sources[0].id;
    let output = cfg.outputs[0].id;
    let hub = build(cfg);
    let client = hub.client();
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    let wheel = connection.filterwheel().unwrap();
    hub.update_simulation(source, patch(json!({"fault":"stalledMotion"})))
        .await
        .unwrap();
    wheel.move_to(6).await.unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(wheel.position().await.unwrap(), -1);
    assert_eq!(wheel.move_to(1).await.unwrap_err().kind, ErrorKind::Busy);
    let before = hub
        .source_snapshot(source)
        .unwrap()
        .simulation
        .unwrap()
        .filter_wheel
        .unwrap()
        .position;
    assert_eq!(
        hub.update_simulation(source, patch(json!({"filterWheel":{"names":["L"]}})))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(before, -1);
    assert_eq!(wheel.position().await.unwrap(), -1);
    hub.update_simulation(
        source,
        patch(json!({"fault":"stoppedShort","filterWheel":{"position":3}})),
    )
    .await
    .unwrap();
    wheel.move_to(1).await.unwrap();
    tokio::time::advance(Duration::from_millis(250)).await;
    assert_eq!(wheel.position().await.unwrap(), 3);
    hub.update_simulation(source, patch(json!({"fault":"invalidMotion"})))
        .await
        .unwrap();
    assert_eq!(
        wheel.position().await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        wheel.move_to(0).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        wheel.property(FilterWheelProperty::Names).await.unwrap(),
        json!(["L", "R", "G", "B", "Hα", "OIII", "SII"])
    );
    hub.update_simulation(source, patch(json!({"fault":"readError"})))
        .await
        .unwrap();
    assert_eq!(
        wheel.position().await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    hub.update_simulation(source, patch(json!({"fault":"none"})))
        .await
        .unwrap();
    assert_eq!(wheel.position().await.unwrap(), 3);
    client.close();
    drop(connection);
    hub.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn wheel_unknown_write_is_applied_once_and_stays_fenced_after_fault_clear() {
    let cfg = config(DeviceType::FilterWheel);
    let source = cfg.sources[0].id;
    let output = cfg.outputs[0].id;
    let hub = build(cfg);
    let a = hub.client();
    let b = hub.client();
    a.connect(output).await.unwrap();
    b.connect(output).await.unwrap();
    let first = a.connection(output).unwrap();
    let second = b.connection(output).unwrap();
    hub.update_simulation(source, patch(json!({"fault":"uncertainWrite"})))
        .await
        .unwrap();
    assert_eq!(
        first
            .filterwheel()
            .unwrap()
            .move_to(2)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    hub.update_simulation(source, patch(json!({"fault":"none"})))
        .await
        .unwrap();
    assert_eq!(
        second
            .filterwheel()
            .unwrap()
            .move_to(1)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    tokio::time::advance(Duration::from_millis(250)).await;
    let state = hub
        .update_simulation(source, SimulationUpdate::default())
        .await
        .unwrap();
    assert_eq!(state.filter_wheel.unwrap().position, 2);
    assert!(hub.source_snapshot(source).unwrap().write_uncertain);
    assert!(!first.connected());
    assert!(!second.connected());
    a.close();
    b.close();
    drop(first);
    drop(second);
    eventually(|| hub.source_snapshot(source).unwrap().lease_count == 0).await;
    let fresh = hub.client();
    fresh.connect(output).await.unwrap();
    let connection = fresh.connection(output).unwrap();
    assert_eq!(
        connection.filterwheel().unwrap().position().await.unwrap(),
        2
    );
    assert!(!hub.source_snapshot(source).unwrap().write_uncertain);
    fresh.close();
    drop(connection);
    hub.shutdown().await.unwrap();
}

#[tokio::test]
async fn wheel_simulation_composes_preserved_metadata_and_age_without_extra_workers() {
    let mut cfg = config(DeviceType::FilterWheel);
    let source = cfg.sources[0].id;
    let base = cfg.outputs[0].id;
    let virtual_source = Uuid::new_v4();
    let output = Uuid::new_v4();
    let revision = cfg.revision;
    cfg.sources.push(serde_json::from_value(json!({"id":virtual_source,"label":"Nested simulated wheel","backend":{"kind":"virtual","output":base},"polling":{"pollSeconds":0.1}})).unwrap());
    cfg.outputs.push(serde_json::from_value(json!({"id":output,"number":17,"label":"Wheel alias","device":{"kind":"proxy","source":virtual_source,"deviceType":"filterwheel"}})).unwrap());
    let hub = build(cfg);
    hub.update_simulation(source,patch(json!({"filterWheel":{"names":["L","Hα",""],"focusOffsets":[-12,0,17]},"sampleAgeSeconds":50}))).await.unwrap();
    eventually(|| hub.source_snapshots().iter().all(|s| s.lease_count == 0)).await;
    assert_eq!(hub.source_snapshot(source).unwrap().revision, revision);
    assert!(hub.outputs().iter().all(|o| o.simulated));
    let a = hub.client();
    let b = hub.client();
    a.connect(base).await.unwrap();
    b.connect(output).await.unwrap();
    let direct = a.connection(base).unwrap();
    let nested = b.connection(output).unwrap();
    assert_eq!(
        nested
            .filterwheel()
            .unwrap()
            .property(FilterWheelProperty::Names)
            .await
            .unwrap(),
        json!(["L", "Hα", ""])
    );
    assert_eq!(
        nested
            .filterwheel()
            .unwrap()
            .property(FilterWheelProperty::FocusOffsets)
            .await
            .unwrap(),
        json!([-12, 0, 17])
    );
    eventually(|| {
        hub.source_snapshot(virtual_source)
            .unwrap()
            .sample_ages_seconds
            .get("position")
            .is_some_and(|age| *age >= 50.0)
    })
    .await;
    assert_eq!(hub.source_snapshot(source).unwrap().lease_count, 2);
    a.close();
    drop(direct);
    assert!(nested.connected());
    nested.filterwheel().unwrap().move_to(2).await.unwrap();
    assert_eq!(nested.filterwheel().unwrap().position().await.unwrap(), -1);
    eventually(|| {
        hub.source_snapshot(source)
            .unwrap()
            .simulation
            .as_ref()
            .unwrap()
            .filter_wheel
            .as_ref()
            .unwrap()
            .position
            == 2
    })
    .await;
    assert_eq!(nested.filterwheel().unwrap().position().await.unwrap(), 2);
    b.close();
    drop(nested);
    hub.shutdown().await.unwrap();
}
