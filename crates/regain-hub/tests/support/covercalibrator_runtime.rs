use super::*;
use regain_hub::{
    config::{HubConfig, NativeDevice, OutputConfig, SourceBackend, SourceConfig, VirtualDevice},
    diagnostics::{Diagnostics, Reading},
    runtime::HubRuntime,
    source::SourceRegistry,
};

fn runtime_setup(device: &Arc<Device>) -> (HubConfig, Arc<HubRuntime>, Arc<SourceHandle>) {
    let mut config = HubConfig::empty();
    let source_id = Uuid::new_v4();
    config.sources.push(SourceConfig {
        id: source_id,
        label: "Private panel".into(),
        polling: PollPolicy {
            request_timeout_seconds: 0.1,
            poll_seconds: 1.0,
            connection_timeout_seconds: 2.0,
            ..Default::default()
        },
        backend: SourceBackend::Native {
            camera: None,
            device: NativeDevice::Ofp2,
            identity: "SIM-OFP2".into(),
            filter_wheel: None,
            temperature_compensation: None,
        },
    });
    for number in [4, 17] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Panel {number}"),
            device: VirtualDevice::Proxy {
                source: source_id,
                device_type: DeviceType::CoverCalibrator,
            },
        });
    }
    let clock = Arc::new(MonotonicClock::default());
    let registry = Arc::new(
        SourceRegistry::build(&config, clock.clone(), |_| {
            Ok(Box::new(Mock(device.clone())))
        })
        .unwrap(),
    );
    let source = registry.get(source_id).unwrap();
    let runtime = HubRuntime::from_registry(config.clone(), registry, clock).unwrap();
    (config, runtime, source)
}
fn properties(
    runtime: &HubRuntime,
    output: Uuid,
) -> Vec<regain_hub::diagnostics::CoverCalibratorProperty> {
    let Diagnostics::CoverCalibrator { properties, .. } =
        runtime.output_status(output, 0, 32).unwrap().diagnostics
    else {
        panic!()
    };
    properties
}
fn reading(
    values: &[regain_hub::diagnostics::CoverCalibratorProperty],
    property: Property,
) -> &regain_hub::covercalibrator::CoverCalibratorSample {
    let Reading::Available { reading } = &values
        .iter()
        .find(|value| value.property == property)
        .unwrap()
        .sample
    else {
        panic!("Missing {property:?}")
    };
    reading
}
async fn poll(source: &SourceHandle, device: &Device) {
    let previous = device.polls.load(SeqCst);
    let mut status = source.status();
    tokio::time::advance(Duration::from_secs(2)).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        while device.polls.load(SeqCst) == previous {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    settle().await;
}
async fn call(stream: &mut tokio::io::DuplexStream, id: u64, command: Value) -> Value {
    use tokio::io::AsyncWriteExt;
    let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .unwrap();
    stream.write_all(&bytes).await.unwrap();
    serde_json::from_slice(
        &regain_hub::ipc::read_frame(stream, Duration::from_secs(2))
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn cached_panel_diagnostics_preserve_independent_errors_and_oldest_light_dependencies_without_io()
 {
    let device = Device::new();
    device.ages.lock().unwrap().extend([
        ("calibratorstate".into(), 10.0),
        ("maxbrightness".into(), 6.0),
        ("brightness".into(), 2.0),
    ]);
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    assert!(
        properties(&runtime, output)
            .iter()
            .all(|p| matches!(p.sample, Reading::Unavailable { .. }))
    );
    assert_eq!(source.snapshot().lease_count, 0);
    assert_eq!(device.connects.load(SeqCst), 0);
    let client = runtime.client();
    client.connect(output).await.unwrap();
    poll(&source, &device).await;
    let before_reads = device.reads.lock().unwrap().len();
    let before_polls = device.polls.load(SeqCst);
    let samples = properties(&runtime, output);
    assert!(reading(&samples, Property::Brightness).age_seconds >= 10.0);
    assert!(reading(&samples, Property::MaxBrightness).age_seconds >= 10.0);
    assert!(reading(&samples, Property::CoverState).age_seconds < 10.0);
    for sample in &samples {
        let Reading::Available { reading } = &sample.sample else {
            panic!()
        };
        assert_eq!(reading.source, config.sources[0].id);
        assert_eq!(reading.generation, source.snapshot().generation);
        assert_eq!(reading.revision, config.revision);
        assert!(reading.sequence <= source.snapshot().sequence);
    }
    assert_eq!(
        runtime.output_status(output, 6, 1).unwrap().next_start,
        None
    );
    assert!(runtime.output_status(output, 7, 1).is_err());
    assert_eq!(device.reads.lock().unwrap().len(), before_reads);
    assert_eq!(device.polls.load(SeqCst), before_polls);
    device.errors.lock().unwrap().insert(
        "coverstate".into(),
        SourceError::new(ErrorKind::Unsupported, "Private cover failure"),
    );
    poll(&source, &device).await;
    let samples = properties(&runtime, output);
    assert!(
        matches!(&samples[2].sample, Reading::Unavailable {error} if error.kind == ErrorKind::Unsupported)
    );
    reading(&samples, Property::Brightness);
    reading(&samples, Property::CalibratorState);
    reading(&samples, Property::CoverMoving);
    let (mut stream, server) = tokio::io::duplex(65536);
    let serving = tokio::spawn(regain_hub::ipc::serve_stream(
        server,
        runtime.clone(),
        regain_hub::ipc::Limits::default(),
    ));
    call(&mut stream, 1, json!({"op":"hello"})).await;
    assert!(
        call(&mut stream, 2, json!({"op":"connect","output":output}))
            .await
            .get("error")
            .is_none()
    );
    let reads = device.reads.lock().unwrap().len();
    let state = call(
        &mut stream,
        3,
        json!({"op":"get","output":output,"property":{"member":"deviceState"}}),
    )
    .await;
    assert_eq!(
        state["result"],
        json!([
        {"Name":"Brightness","Value":0},{"Name":"CalibratorChanging","Value":false},
        {"Name":"CalibratorState","Value":1},{"Name":"CoverMoving","Value":false}])
    );
    assert_eq!(device.reads.lock().unwrap().len(), reads);
    drop(stream);
    serving.await.unwrap().unwrap();
    device.errors.lock().unwrap().insert(
        "covermoving".into(),
        SourceError::new(ErrorKind::Unsupported, "Modern completion failure"),
    );
    poll(&source, &device).await;
    assert!(
        matches!(&properties(&runtime,output)[4].sample, Reading::Unavailable {error} if error.kind == ErrorKind::Unsupported)
    );
    device.set("maxbrightness", json!(0));
    poll(&source, &device).await;
    let samples = properties(&runtime, output);
    assert!(matches!(samples[0].sample, Reading::Unavailable { .. }));
    assert!(matches!(samples[1].sample, Reading::Unavailable { .. }));
    reading(&samples, Property::CalibratorState);
    reading(&samples, Property::CalibratorChanging);
    device.set("maxbrightness", json!(4096));
    device.errors.lock().unwrap().clear();
    poll(&source, &device).await;
    assert!(
        properties(&runtime, output)
            .iter()
            .all(|p| matches!(p.sample, Reading::Available { .. }))
    );
    client.close();
    runtime.shutdown().await.unwrap();
    assert!(device.writes.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn legacy_cached_completion_ignores_unsupported_new_properties_without_inventing_unknown_completion()
 {
    let device = Device::new();
    device.version.store(1, SeqCst);
    for member in ["covermoving", "calibratorchanging"] {
        device.errors.lock().unwrap().insert(
            member.into(),
            SourceError::new(ErrorKind::Unsupported, "Legacy property absent"),
        );
    }
    device.set("coverstate", json!(4));
    device.set("calibratorstate", json!(3));
    device.set("brightness", json!(17));
    device
        .ages
        .lock()
        .unwrap()
        .insert("calibratorstate".into(), 10.0);
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let client = runtime.client();
    client.connect(output).await.unwrap();
    poll(&source, &device).await;
    let samples = properties(&runtime, output);
    assert!(
        matches!(&samples[4].sample, Reading::Unavailable {error} if error.kind == ErrorKind::Unavailable)
    );
    let completion = reading(&samples, Property::CalibratorChanging);
    assert!(matches!(
        completion.value,
        regain_hub::covercalibrator::CoverCalibratorValue::Boolean { value: false }
    ));
    assert!(completion.age_seconds >= 10.0);
    device.set("calibratorstate", json!(0));
    poll(&source, &device).await;
    let samples = properties(&runtime, output);
    assert!(
        matches!(&samples[0].sample, Reading::Unavailable {error} if error.kind == ErrorKind::Unsupported)
    );
    assert!(
        matches!(&samples[1].sample, Reading::Unavailable {error} if error.kind == ErrorKind::Unsupported)
    );
    reading(&samples, Property::CalibratorChanging);
    client.close();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn panel_ipc_shares_sparse_outputs_actual_state_and_unknown_write_fences_without_cleanup_commands()
 {
    use regain_hub::ipc::{Limits, serve_stream};
    let device = Device::new();
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let sibling = config.outputs[1].id;
    let (mut first, a_server) = tokio::io::duplex(65536);
    let (mut second, b_server) = tokio::io::duplex(65536);
    let a = tokio::spawn(serve_stream(a_server, runtime.clone(), Limits::default()));
    let b = tokio::spawn(serve_stream(b_server, runtime.clone(), Limits::default()));
    let hello = call(&mut first, 1, json!({"op":"hello"})).await;
    assert!(
        hello["result"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "coverCalibratorOutputs")
    );
    call(&mut second, 1, json!({"op":"hello"})).await;
    let catalog = call(&mut first, 2, json!({"op":"listDevices"})).await;
    assert_eq!(catalog["result"][0]["number"], 4);
    assert_eq!(catalog["result"][1]["number"], 17);
    assert!(
        call(&mut first, 3, json!({"op":"connect","output":output}))
            .await
            .get("error")
            .is_none()
    );
    assert!(
        call(&mut second, 2, json!({"op":"connect","output":sibling}))
            .await
            .get("error")
            .is_none()
    );
    assert_eq!(device.connects.load(SeqCst), 1);
    assert_eq!(source.snapshot().lease_count, 2);
    assert_eq!(call(&mut first, 4, json!({"op":"get","output":output,"property":{"member":"filterWheel","property":"position"}})).await["error"]["code"], "unsupported");
    assert_eq!(call(&mut first, 5, json!({"op":"put","output":output,"property":{"member":"calibratorOn","brightness":4097}})).await["error"]["code"], "invalidValue");
    assert!(device.writes.lock().unwrap().is_empty());
    assert!(
        call(
            &mut first,
            6,
            json!({"op":"put","output":output,"property":{"member":"calibratorOn","brightness":0}})
        )
        .await
        .get("error")
        .is_none()
    );
    assert_eq!(call(&mut second, 3, json!({"op":"get","output":sibling,"property":{"member":"coverCalibrator","property":"calibratorState"}})).await["result"], 2);
    assert!(
        call(
            &mut first,
            7,
            json!({"op":"put","output":output,"property":{"member":"openCover"}})
        )
        .await
        .get("error")
        .is_none()
    );
    poll(&source, &device).await;
    let reads = device.reads.lock().unwrap().len();
    let state = call(
        &mut second,
        4,
        json!({"op":"get","output":sibling,"property":{"member":"deviceState"}}),
    )
    .await;
    assert_eq!(
        state["result"],
        json!([
        {"Name":"Brightness","Value":0},{"Name":"CalibratorChanging","Value":true},{"Name":"CalibratorState","Value":2},
        {"Name":"CoverMoving","Value":true},{"Name":"CoverState","Value":2}])
    );
    assert_eq!(device.reads.lock().unwrap().len(), reads);
    assert!(
        call(
            &mut first,
            8,
            json!({"op":"put","output":output,"property":{"member":"haltCover"}})
        )
        .await
        .get("error")
        .is_none()
    );
    assert_eq!(call(&mut second, 5, json!({"op":"get","output":sibling,"property":{"member":"coverCalibrator","property":"coverState"}})).await["result"], 4);
    drop(first);
    a.await.unwrap().unwrap();
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.disconnects.load(SeqCst), 0);
    device.uncertain.store(true, SeqCst);
    assert_eq!(
        call(
            &mut second,
            6,
            json!({"op":"put","output":sibling,"property":{"member":"closeCover"}})
        )
        .await["error"]["code"],
        "uncertain"
    );
    assert_eq!(
        call(
            &mut second,
            7,
            json!({"op":"put","output":sibling,"property":{"member":"calibratorOff"}})
        )
        .await["error"]["code"],
        "uncertain"
    );
    assert!(source.snapshot().write_uncertain);
    assert_eq!(device.writes.lock().unwrap().len(), 4);
    drop(second);
    b.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 4);
}

#[tokio::test(start_paused = true)]
async fn pending_panel_runtime_admission_and_lost_generation_release_only_owned_activity() {
    let device = Device::new();
    device.pending.store(true, SeqCst);
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let client = runtime.client();
    let pending = tokio::spawn({
        let client = client.clone();
        async move { client.connect(output).await }
    });
    settle().await;
    assert!(client.connecting(output).unwrap());
    assert_eq!(runtime.active_connections(), 1);
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    settle().await;
    assert_eq!(runtime.active_connections(), 0);
    assert_eq!(source.snapshot().lease_count, 0);
    device.pending.store(false, SeqCst);
    client.connect(output).await.unwrap();
    let connection = client.connection(output).unwrap();
    let generation = connection.covercalibrator().unwrap().generation();
    *device.hang_read.lock().unwrap() = Some("maxbrightness".into());
    assert_eq!(
        connection
            .covercalibrator()
            .unwrap()
            .calibrator_on(17)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Transient
    );
    assert!(!connection.connected());
    assert_ne!(source.snapshot().generation, generation);
    assert!(device.writes.lock().unwrap().is_empty());
    assert!(!source.snapshot().write_uncertain);
    *device.hang_read.lock().unwrap() = None;
    drop(connection);
    client.disconnect(output);
    settle().await;
    client.connect(output).await.unwrap();
    client
        .connection(output)
        .unwrap()
        .covercalibrator()
        .unwrap()
        .calibrator_on(17)
        .await
        .unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    client.close();
    runtime.shutdown().await.unwrap();
}
