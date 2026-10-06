use super::*;
use regain_hub::{
    config::{
        DeviceType, HubConfig, NativeDevice, OutputConfig, SourceBackend, SourceConfig,
        VirtualDevice,
    },
    diagnostics::{Diagnostics, Reading},
    runtime::HubRuntime,
    source::SourceRegistry,
};

fn wheel_config() -> HubConfig {
    let mut config = HubConfig::empty();
    let source = Uuid::new_v4();
    config.sources.push(SourceConfig {
        id: source,
        label: "Private wheel".into(),
        polling: PollPolicy {
            request_timeout_seconds: 0.1,
            poll_seconds: 1.0,
            connection_timeout_seconds: 2.0,
            ..Default::default()
        },
        backend: SourceBackend::Native {
            device: NativeDevice::Efw,
            identity: "0102030405060708".into(),
            filter_wheel: None,
        },
    });
    for number in [4, 17] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Wheel {number}"),
            device: VirtualDevice::Proxy {
                source,
                device_type: DeviceType::FilterWheel,
            },
        });
    }
    config
}
fn runtime_setup(device: &Arc<Device>) -> (HubConfig, Arc<HubRuntime>, Arc<SourceHandle>) {
    let config = wheel_config();
    let clock = Arc::new(MonotonicClock::default());
    let registry = Arc::new(
        SourceRegistry::build(&config, clock.clone(), |_| {
            Ok(Box::new(Mock(device.clone())))
        })
        .unwrap(),
    );
    let source = registry.get(config.sources[0].id).unwrap();
    let runtime = HubRuntime::from_registry(config.clone(), registry, clock).unwrap();
    (config, runtime, source)
}
async fn wait_sample(source: &SourceHandle, position: i32) {
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(3), async {
        while status.borrow_and_update().values.get("position") != Some(&json!(position)) {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
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
async fn cached_wheel_diagnostics_are_inert_and_validate_metadata_dependencies_and_ages() {
    let device = Device::new();
    device
        .ages
        .lock()
        .unwrap()
        .insert("focusoffsets".into(), 5.0);
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let inactive = runtime.output_status(output, 0, 1).unwrap();
    assert_eq!(inactive.total, 3);
    assert_eq!(inactive.next_start, Some(1));
    assert_eq!(source.snapshot().lease_count, 0);
    assert_eq!(device.connects.load(SeqCst), 0);
    assert_eq!(device.reads.load(SeqCst), 0);
    assert_eq!(device.polls.load(SeqCst), 0);
    let client = runtime.client();
    client.connect(output).await.unwrap();
    wait_sample(&source, 0).await;
    let reads = device.reads.load(SeqCst);
    let polls = device.polls.load(SeqCst);
    let Diagnostics::FilterWheel { health, properties } =
        runtime.output_status(output, 0, 32).unwrap().diagnostics
    else {
        panic!()
    };
    assert_eq!(health.source, source.snapshot().source);
    for property in &properties {
        let Reading::Available { reading } = &property.sample else {
            panic!()
        };
        assert!(reading.age_seconds >= 5.0);
        assert_eq!(reading.generation, health.generation);
        assert_eq!(reading.revision, config.revision);
        assert!(reading.sequence <= health.sequence);
    }
    assert!(runtime.output_status(output, 4, 1).is_err());
    assert_eq!(
        runtime.output_status(output, 3, 1).unwrap().next_start,
        None
    );
    assert_eq!(device.reads.load(SeqCst), reads);
    assert_eq!(device.polls.load(SeqCst), polls);
    // A malformed dependency invalidates the Position bounds too, without a
    // getter probing the upstream to repair the cache.
    device.set("focusoffsets", json!([0]));
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    let Diagnostics::FilterWheel { properties, .. } =
        runtime.output_status(output, 0, 32).unwrap().diagnostics
    else {
        panic!()
    };
    assert!(
        properties
            .iter()
            .all(|property| matches!(property.sample, Reading::Unavailable { .. }))
    );
    device.set("focusoffsets", json!([-12, 0, 17]));
    device.set("position", json!(-1));
    tokio::time::advance(Duration::from_secs(2)).await;
    wait_sample(&source, -1).await;
    let Diagnostics::FilterWheel { properties, .. } =
        runtime.output_status(output, 0, 32).unwrap().diagnostics
    else {
        panic!()
    };
    assert!(
        properties
            .iter()
            .all(|property| matches!(property.sample, Reading::Available { .. }))
    );
    device.errors.lock().unwrap().insert(
        "position".into(),
        SourceError::new(ErrorKind::Unsupported, "Private Position failure"),
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    settle().await;
    let Diagnostics::FilterWheel { properties, .. } =
        runtime.output_status(output, 0, 32).unwrap().diagnostics
    else {
        panic!()
    };
    assert!(
        properties[..2]
            .iter()
            .all(|property| matches!(property.sample, Reading::Available { .. }))
    );
    assert!(
        matches!(&properties[2].sample,Reading::Unavailable{error} if error.kind == ErrorKind::Unsupported)
    );
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
    let reads = device.reads.load(SeqCst);
    let state = call(
        &mut stream,
        3,
        json!({"op":"get","output":output,"property":{"member":"deviceState"}}),
    )
    .await;
    assert_eq!(state["result"], json!([]));
    assert_eq!(device.reads.load(SeqCst), reads);
    drop(stream);
    serving.await.unwrap().unwrap();
    device.errors.lock().unwrap().clear();
    tokio::time::advance(Duration::from_secs(2)).await;
    wait_sample(&source, -1).await;
    let Diagnostics::FilterWheel { properties, .. } =
        runtime.output_status(output, 0, 32).unwrap().diagnostics
    else {
        panic!()
    };
    assert!(
        properties
            .iter()
            .all(|property| matches!(property.sample, Reading::Available { .. }))
    );
    let reads = device.reads.load(SeqCst);
    assert_eq!(
        client
            .connection(output)
            .unwrap()
            .filterwheel()
            .unwrap()
            .generation(),
        source.snapshot().generation
    );
    assert_eq!(device.reads.load(SeqCst), reads);
    assert!(device.writes.lock().unwrap().is_empty());
    client.close();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn pending_wheel_connection_and_preflight_generation_loss_release_admission_without_replay() {
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
    let generation = connection.filterwheel().unwrap().generation();
    *device.hang_read.lock().unwrap() = Some("names".into());
    assert_eq!(
        connection
            .filterwheel()
            .unwrap()
            .move_to(2)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Transient
    );
    assert!(!connection.connected());
    assert_ne!(source.snapshot().generation, generation);
    assert!(!source.snapshot().write_uncertain);
    assert!(device.writes.lock().unwrap().is_empty());
    *device.hang_read.lock().unwrap() = None;
    drop(connection);
    client.disconnect(output);
    settle().await;
    client.connect(output).await.unwrap();
    client
        .connection(output)
        .unwrap()
        .filterwheel()
        .unwrap()
        .move_to(2)
        .await
        .unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    client.close();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn wheel_ipc_preserves_arrays_sparse_ids_sibling_leases_and_unknown_move_fencing() {
    use regain_hub::ipc::{Limits, serve_stream};
    let device = Device::new();
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let sibling = config.outputs[1].id;
    let (mut first, first_server) = tokio::io::duplex(65536);
    let (mut second, second_server) = tokio::io::duplex(65536);
    let a = tokio::spawn(serve_stream(
        first_server,
        runtime.clone(),
        Limits::default(),
    ));
    let b = tokio::spawn(serve_stream(
        second_server,
        runtime.clone(),
        Limits::default(),
    ));
    let hello = call(&mut first, 1, json!({"op":"hello"})).await;
    assert!(
        hello["result"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "filterWheelOutputs")
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
    assert_eq!(source.snapshot().lease_count, 2);
    assert_eq!(device.connects.load(SeqCst), 1);
    let names = call(
        &mut first,
        4,
        json!({"op":"get","output":output,"property":{"member":"filterWheel","property":"names"}}),
    )
    .await;
    assert_eq!(names["result"], json!(["L", "Hα", ""]));
    let offsets=call(&mut first,5,json!({"op":"get","output":output,"property":{"member":"filterWheel","property":"focusOffsets"}})).await;
    assert_eq!(offsets["result"], json!([-12, 0, 17]));
    let wrong = call(
        &mut first,
        6,
        json!({"op":"get","output":output,"property":{"member":"focuser","property":"position"}}),
    )
    .await;
    assert_eq!(wrong["error"]["code"], "unsupported");
    let invalid = call(
        &mut first,
        7,
        json!({"op":"put","output":output,"property":{"member":"moveFilterWheel","position":3}}),
    )
    .await;
    assert_eq!(invalid["error"]["code"], "invalidValue");
    assert!(device.writes.lock().unwrap().is_empty());
    assert!(
        call(
            &mut first,
            8,
            json!({"op":"put","output":output,"property":{"member":"moveFilterWheel","position":2}})
        )
        .await
        .get("error")
        .is_none()
    );
    let moving=call(&mut second,3,json!({"op":"get","output":sibling,"property":{"member":"filterWheel","property":"position"}})).await;
    assert_eq!(moving["result"], -1);
    wait_sample(&source, -1).await;
    let reads = device.reads.load(SeqCst);
    let state = call(
        &mut second,
        4,
        json!({"op":"get","output":sibling,"property":{"member":"deviceState"}}),
    )
    .await;
    assert_eq!(state["result"], json!([{"Name":"Position","Value":-1}]));
    assert_eq!(device.reads.load(SeqCst), reads);
    drop(first);
    a.await.unwrap().unwrap();
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.disconnects.load(SeqCst), 0);
    device.set("position", json!(2));
    device.uncertain.store(true, SeqCst);
    let uncertain = call(
        &mut second,
        5,
        json!({"op":"put","output":sibling,"property":{"member":"moveFilterWheel","position":1}}),
    )
    .await;
    assert_eq!(uncertain["error"]["code"], "uncertain");
    let fenced = call(
        &mut second,
        6,
        json!({"op":"put","output":sibling,"property":{"member":"moveFilterWheel","position":0}}),
    )
    .await;
    assert_eq!(fenced["error"]["code"], "uncertain");
    assert!(source.snapshot().write_uncertain);
    let connected = call(
        &mut second,
        7,
        json!({"op":"get","output":sibling,"property":{"member":"connected"}}),
    )
    .await;
    assert_eq!(connected["result"], false);
    assert_eq!(device.writes.lock().unwrap().len(), 2);
    drop(second);
    b.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn oversized_wheel_ipc_metadata_is_explicit_and_does_not_close_or_mutate_the_session() {
    use regain_hub::ipc::{Limits, serve_stream};
    let device = Device::new();
    device.set("names", json!(["\u{1}".repeat(512 * 1024)]));
    device.set("focusoffsets", json!([0]));
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let (mut stream, server) = tokio::io::duplex(65536);
    let serving = tokio::spawn(serve_stream(server, runtime.clone(), Limits::default()));
    call(&mut stream, 1, json!({"op":"hello"})).await;
    assert!(
        call(&mut stream, 2, json!({"op":"connect","output":output}))
            .await
            .get("error")
            .is_none()
    );
    let large = call(
        &mut stream,
        3,
        json!({"op":"get","output":output,"property":{"member":"filterWheel","property":"names"}}),
    )
    .await;
    assert_eq!(large["error"]["code"], "responseTooLarge");
    wait_sample(&source, 0).await;
    let diagnostic = call(
        &mut stream,
        4,
        json!({"op":"outputStatus","output":output,
        "expectedRevision":config.revision,"start":0,"limit":3}),
    )
    .await;
    assert_eq!(diagnostic["error"]["code"], "responseTooLarge");
    let position=call(&mut stream,5,json!({"op":"get","output":output,"property":{"member":"filterWheel","property":"position"}})).await;
    assert_eq!(position["result"], 0);
    assert_eq!(source.snapshot().lease_count, 1);
    assert!(!source.snapshot().write_uncertain);
    assert!(device.writes.lock().unwrap().is_empty());
    drop(stream);
    serving.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}
