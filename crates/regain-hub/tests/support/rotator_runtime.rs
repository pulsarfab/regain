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

#[tokio::test(start_paused = true)]
async fn modern_rotator_admission_requires_reversal_and_bounds_the_whole_readiness_check() {
    for case in [
        "cannotreverse",
        "missingreverse",
        "invalidreverse",
        "hungreverse",
    ] {
        let device = Device::new();
        // A request longer than the handshake proves that the outer deadline
        // also covers required property reads, not just transport connection.
        let (config, host, source) = runtime_setup_with_request_timeout(
            &device,
            if case == "hungreverse" { 30.0 } else { 0.1 },
        );
        match case {
            "cannotreverse" => {
                device.set("canreverse", json!(false));
                let controller =
                    RotatorController::new(source.clone(), Duration::from_secs(2)).unwrap();
                let legacy = controller.connect().await.unwrap();
                assert!(!legacy.capabilities().await.unwrap().can_reverse);
                drop(legacy);
                settle().await;
            }
            "missingreverse" => {
                device.errors.lock().unwrap().insert(
                    "reverse".into(),
                    SourceError::new(ErrorKind::Unsupported, "Source cannot report direction"),
                );
            }
            "invalidreverse" => device.set("reverse", json!(1)),
            "hungreverse" => {
                *device.hang_read.lock().unwrap() = Some("reverse".into());
            }
            _ => unreachable!(),
        }
        let client = host.client();
        let started = tokio::time::Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs(3), client.connect(config.outputs[0].id))
                .await
                .unwrap();
        assert!(result.is_err(), "{case}");
        if case == "hungreverse" {
            assert_eq!(started.elapsed(), Duration::from_secs(2));
        }
        settle().await;
        assert_eq!(host.active_connections(), 0, "{case}");
        // Dropping a session queues its release behind the bounded actor read.
        let mut status = source.status();
        tokio::time::timeout(Duration::from_secs(31), async {
            while status.borrow_and_update().lease_count != 0 {
                status.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(source.snapshot().lease_count, 0, "{case}");
        assert!(device.writes.lock().unwrap().is_empty());
        client.close();
        host.shutdown().await.unwrap();
    }
}

fn runtime_setup(device: &Arc<Device>) -> (HubConfig, Arc<HubRuntime>, Arc<SourceHandle>) {
    runtime_setup_with_request_timeout(device, 0.1)
}
fn runtime_setup_with_request_timeout(
    device: &Arc<Device>,
    request_timeout_seconds: f64,
) -> (HubConfig, Arc<HubRuntime>, Arc<SourceHandle>) {
    let mut config = HubConfig::empty();
    let source_id = Uuid::new_v4();
    config.sources.push(SourceConfig {
        id: source_id,
        label: "Private rotator".into(),
        polling: PollPolicy {
            request_timeout_seconds,
            poll_seconds: 1.0,
            connection_timeout_seconds: 2.0,
            ..PollPolicy::default()
        },
        backend: SourceBackend::Native {
            device: NativeDevice::Caa,
            identity: "0102030405060708".into(),
        },
    });
    for number in [17, 42] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Rotator {number}"),
            device: VirtualDevice::Proxy {
                source: source_id,
                device_type: DeviceType::Rotator,
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
async fn wait_samples(source: &SourceHandle) {
    let mut status = source.status();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !status
            .borrow_and_update()
            .values
            .contains_key("targetposition")
        {
            status.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test(start_paused = true)]
async fn cached_rotator_diagnostics_preserve_separate_angles_errors_ages_and_inert_paging() {
    let device = Device::new();
    let (config, runtime, source) = runtime_setup(&device);
    let output = config.outputs[0].id;
    let inactive = runtime.output_status(output, 0, 3).unwrap();
    assert_eq!(inactive.total, 7);
    assert_eq!(inactive.next_start, Some(3));
    assert_eq!(device.connects.load(SeqCst), 0);
    assert_eq!(device.reads.load(SeqCst), 0);
    assert_eq!(device.polls.load(SeqCst), 0);
    assert_eq!(source.snapshot().lease_count, 0);
    let client = runtime.client();
    client.connect(output).await.unwrap();
    wait_samples(&source).await;
    let first_reads = device.reads.load(SeqCst);
    let first_polls = device.polls.load(SeqCst);
    let first = runtime.output_status(output, 0, 3).unwrap();
    let Diagnostics::Rotator { health, properties } = first.diagnostics else {
        panic!()
    };
    assert_eq!(
        health.generation,
        client
            .connection(output)
            .unwrap()
            .rotator()
            .unwrap()
            .generation()
    );
    assert!(
        properties
            .iter()
            .all(|item| matches!(item.sample, Reading::Available { .. }))
    );
    let connection = client.connection(output).unwrap();
    assert_eq!(device.reads.load(SeqCst), first_reads);
    assert_eq!(device.polls.load(SeqCst), first_polls);
    let cached = device_state(&runtime, output, &device).await;
    let reads = device.reads.load(SeqCst);
    let polls = device.polls.load(SeqCst);
    let values: Values = cached
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["Name"].as_str().unwrap().into(), item["Value"].clone()))
        .collect();
    assert_eq!(values["MechanicalPosition"], 350.0);
    assert_eq!(values["Position"], 20.0);
    assert_eq!(values.len(), 3);
    assert!(!values.contains_key("TargetPosition"));
    assert!(!values.contains_key("Reverse"));
    tokio::time::advance(Duration::from_millis(100)).await;
    let Diagnostics::Rotator { properties, .. } =
        runtime.output_status(output, 3, 32).unwrap().diagnostics
    else {
        panic!()
    };
    let Reading::Available { reading } = &properties[0].sample else {
        panic!()
    };
    assert!(reading.age_seconds >= 0.1);
    assert_eq!(device.reads.load(SeqCst), reads);
    assert_eq!(device.polls.load(SeqCst), polls);
    assert!(runtime.output_status(output, 8, 1).is_err());
    device.errors.lock().unwrap().insert(
        "stepsize".into(),
        SourceError::new(ErrorKind::Unsupported, "Optional property is unavailable"),
    );
    device.set("position", json!(360.0));
    device.set("ismoving", json!(1));
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    let Diagnostics::Rotator { properties, .. } =
        runtime.output_status(output, 0, 32).unwrap().diagnostics
    else {
        panic!()
    };
    for index in [1, 3, 5] {
        assert!(matches!(
            properties[index].sample,
            Reading::Unavailable { .. }
        ));
    }
    let state = device_state(&runtime, output, &device).await;
    assert!(
        !state
            .as_array()
            .unwrap()
            .iter()
            .any(|item| ["Position", "IsMoving"].contains(&item["Name"].as_str().unwrap()))
    );
    assert!(
        state
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["Name"] == "MechanicalPosition" && item["Value"] == 350.0)
    );
    drop(connection);
    client.close();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn pending_rotator_connection_and_read_generation_loss_do_not_leave_admission_or_adopt_sessions()
 {
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
    let generation = connection.rotator().unwrap().generation();
    *device.hang_read.lock().unwrap() = Some("ismoving".into());
    assert_eq!(
        connection
            .rotator()
            .unwrap()
            .move_relative(12.5)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Transient
    );
    assert!(!connection.connected());
    assert_ne!(source.snapshot().generation, generation);
    assert!(device.writes.lock().unwrap().is_empty());
    *device.hang_read.lock().unwrap() = None;
    drop(connection);
    client.disconnect(output);
    settle().await;
    client.connect(output).await.unwrap();
    client
        .connection(output)
        .unwrap()
        .rotator()
        .unwrap()
        .move_absolute(33.5)
        .await
        .unwrap();
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    client.close();
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn actual_rotator_ipc_dispatch_preserves_sparse_identity_shared_leases_and_uncertain_no_replay()
 {
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
            .any(|item| item == "rotatorOutputs")
    );
    call(&mut second, 1, json!({"op":"hello"})).await;
    let catalog = call(&mut first, 2, json!({"op":"listDevices"})).await;
    assert_eq!(catalog["result"][0]["number"], 17);
    assert_eq!(catalog["result"][1]["number"], 42);
    let connect = call(&mut first, 3, json!({"op":"connect","output":output})).await;
    assert!(connect.get("error").is_none());
    let connect = call(&mut second, 2, json!({"op":"connect","output":sibling})).await;
    assert!(connect.get("error").is_none());
    assert_eq!(device.connects.load(SeqCst), 1);
    assert_eq!(source.snapshot().lease_count, 2);
    let wrong = call(
        &mut first,
        4,
        json!({"op":"get","output":output,"property":{"member":"focuser","property":"position"}}),
    )
    .await;
    assert_eq!(wrong["error"]["code"], "unsupported");
    let invalid = call(&mut first, 5, json!({"op":"put","output":output,"property":{"member":"moveAbsoluteRotator","degrees":360.0}})).await;
    assert_eq!(invalid["error"]["code"], "invalidValue");
    assert!(device.writes.lock().unwrap().is_empty());
    for (id, property) in [
        (6, json!({"member":"syncRotator","degrees":42.5})),
        (7, json!({"member":"rotatorReverse","enabled":true})),
        (8, json!({"member":"moveMechanicalRotator","degrees":12.25})),
        (9, json!({"member":"haltRotator"})),
        (10, json!({"member":"moveRotator","degrees":-721.5})),
    ] {
        let reply = call(
            &mut first,
            id,
            json!({"op":"put","output":output,"property":property}),
        )
        .await;
        assert!(reply.get("error").is_none(), "{reply}");
    }
    let moving = call(
        &mut second,
        3,
        json!({"op":"get","output":sibling,"property":{"member":"rotator","property":"isMoving"}}),
    )
    .await;
    assert_eq!(moving["result"], true);
    drop(first);
    a.await.unwrap().unwrap();
    settle().await;
    assert_eq!(source.snapshot().lease_count, 1);
    assert_eq!(device.disconnects.load(SeqCst), 0);
    assert_eq!(device.writes.lock().unwrap().len(), 5);
    call(
        &mut second,
        4,
        json!({"op":"put","output":sibling,"property":{"member":"haltRotator"}}),
    )
    .await;
    device.uncertain.store(true, SeqCst);
    let uncertain = call(&mut second, 5, json!({"op":"put","output":sibling,"property":{"member":"moveAbsoluteRotator","degrees":123.25}})).await;
    assert_eq!(uncertain["error"]["code"], "uncertain");
    let connected = call(
        &mut second,
        6,
        json!({"op":"get","output":sibling,"property":{"member":"connected"}}),
    )
    .await;
    assert_eq!(connected["result"], false);
    let fenced = call(&mut second, 7, json!({"op":"put","output":sibling,"property":{"member":"moveAbsoluteRotator","degrees":125.0}})).await;
    assert_eq!(fenced["error"]["code"], "uncertain");
    assert_eq!(device.writes.lock().unwrap().len(), 7);
    assert!(source.snapshot().write_uncertain);
    drop(second);
    b.await.unwrap().unwrap();
    settle().await;
    assert_eq!(runtime.active_connections(), 0);
    assert_eq!(source.snapshot().lease_count, 0);
    assert!(!source.snapshot().write_uncertain);
    assert_eq!(device.writes.lock().unwrap().len(), 7);
    runtime.shutdown().await.unwrap();
}

#[test]
fn rotator_poll_plans_deduplicate_all_typed_properties_without_connecting_and_setup_stays_gated() {
    let device = Device::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (config, host, _) = runtime_setup(&device);
        let plans = regain_hub::factory::source_plans(&config).unwrap();
        let plan = &plans[&config.sources[0].id];
        assert_eq!(plan.samples.len(), 7);
        for sample in &plan.samples {
            assert!(sample.parameters.is_empty());
            assert!(sample.sensor_age.is_none());
            let boolean = ["canreverse", "ismoving", "reverse"].contains(&sample.member.as_str());
            assert_eq!(
                matches!(sample.value_type, regain_hub::sampling::SampleType::Boolean),
                boolean
            );
        }
        use regain_hub::ipc::{Limits, read_frame, serve_stream};
        use tokio::io::AsyncWriteExt;
        let (mut stream, server) = tokio::io::duplex(1024 * 1024);
        let task = tokio::spawn(serve_stream(server, host.clone(), Limits::default()));
        for (id, op) in [(1, "hello"), (2, "describeConfig")] {
            let bytes =
                serde_json::to_vec(&json!({"version":1,"id":id,"command":{"op":op}})).unwrap();
            stream
                .write_all(&(bytes.len() as u32).to_le_bytes())
                .await
                .unwrap();
            stream.write_all(&bytes).await.unwrap();
            let reply: Value = serde_json::from_slice(
                &read_frame(&mut stream, Duration::from_secs(2))
                    .await
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            if id == 2 {
                assert!(
                    !reply["result"]["capabilities"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|item| item == "rotatorOutputs")
                );
            }
        }
        drop(stream);
        task.await.unwrap().unwrap();
        assert_eq!(device.connects.load(SeqCst), 0);
        host.shutdown().await.unwrap();
    });
}

async fn device_state(runtime: &Arc<HubRuntime>, output: Uuid, device: &Arc<Device>) -> Value {
    use regain_hub::ipc::{Limits, read_frame, serve_stream};
    use tokio::io::AsyncWriteExt;
    let (mut stream, server) = tokio::io::duplex(65536);
    let task = tokio::spawn(serve_stream(server, runtime.clone(), Limits::default()));
    let mut result = Value::Null;
    for (id, command) in [
        (1, json!({"op":"hello"})),
        (2, json!({"op":"connect","output":output})),
        (
            3,
            json!({"op":"get","output":output,"property":{"member":"deviceState"}}),
        ),
    ] {
        let reads = device.reads.load(SeqCst);
        let polls = device.polls.load(SeqCst);
        let bytes = serde_json::to_vec(&json!({"version":1,"id":id,"command":command})).unwrap();
        stream
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(&bytes).await.unwrap();
        let reply: Value = serde_json::from_slice(
            &read_frame(&mut stream, Duration::from_secs(2))
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert!(reply.get("error").is_none(), "{reply}");
        result = reply["result"].clone();
        if id == 3 {
            assert_eq!(device.reads.load(SeqCst), reads);
            assert_eq!(device.polls.load(SeqCst), polls);
        }
    }
    drop(stream);
    task.await.unwrap().unwrap();
    settle().await;
    result
}

async fn call(stream: &mut tokio::io::DuplexStream, id: u64, command: Value) -> Value {
    use regain_hub::ipc::read_frame;
    use tokio::io::AsyncWriteExt;
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

#[tokio::test]
async fn tracked_relative_receipt_holds_command_ownership_until_target_read_and_never_replays_failed_readback()
 {
    use regain_hub::ipc::{Limits, serve_stream};
    let device = Device::new();
    let (config, runtime, source) = runtime_setup_with_request_timeout(&device, 5.0);
    let output = config.outputs[0].id;
    let sibling = config.outputs[1].id;
    let (mut first, server) = tokio::io::duplex(65536);
    let a = tokio::spawn(serve_stream(server, runtime.clone(), Limits::default()));
    let (mut second, server) = tokio::io::duplex(65536);
    let b = tokio::spawn(serve_stream(server, runtime.clone(), Limits::default()));
    let hello = call(&mut first, 1, json!({"op":"hello"})).await;
    assert!(
        hello["result"]["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "rotatorMotionReceipt")
    );
    call(&mut second, 1, json!({"op":"hello"})).await;
    for (stream, output) in [(&mut first, output), (&mut second, sibling)] {
        let reply = call(stream, 2, json!({"op":"connect","output":output})).await;
        assert!(reply.get("error").is_none(), "{reply}");
    }
    device.hold_target.store(true, SeqCst);
    let moving = tokio::spawn(async move {
        let reply = call(&mut first, 3, json!({"op":"put","output":output,"property":{"member":"moveRotatorTracked","degrees":-721.5}})).await;
        (first, reply)
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while !device.target_reading.load(SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Two connected outputs plus the still-owned exclusive command lease.
    // Releasing control before receipt would leave only two leases here.
    assert_eq!(source.snapshot().lease_count, 3);
    device.set("ismoving", json!(false));
    let other = tokio::spawn(async move {
        let reply = call(
            &mut second,
            3,
            json!({"op":"put","output":sibling,"property":{"member":"syncRotator","degrees":84.0}}),
        )
        .await;
        (second, reply)
    });
    settle().await;
    assert_eq!(device.writes.lock().unwrap().len(), 1);
    device.release_target.notify_one();
    let (mut first, receipt) = moving.await.unwrap();
    assert_eq!(
        receipt["result"],
        json!({"expectedTarget":18.5,"targetPosition":18.5})
    );
    let (mut second, other_reply) = other.await.unwrap();
    if other_reply.get("error").is_some() {
        assert_eq!(other_reply["error"]["code"], "busy");
        settle().await;
        let reply = call(
            &mut second,
            4,
            json!({"op":"put","output":sibling,"property":{"member":"syncRotator","degrees":84.0}}),
        )
        .await;
        assert!(reply.get("error").is_none(), "{reply}");
    }
    assert_eq!(device.values.lock().unwrap()["position"], 84.0);
    device.hold_target.store(false, SeqCst);
    device.errors.lock().unwrap().insert(
        "targetposition".into(),
        SourceError::new(ErrorKind::Unsupported, "No target readback"),
    );
    let failed = call(&mut first, 4, json!({"op":"put","output":output,"property":{"member":"moveRotatorTracked","degrees":5.0}})).await;
    assert_eq!(failed["error"]["code"], "unavailable");
    assert!(
        failed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("move was accepted")
    );
    assert!(!source.snapshot().write_uncertain); // The write ACK was unambiguous.
    {
        let writes = device.writes.lock().unwrap();
        assert_eq!(
            writes.iter().filter(|(member, _)| member == "move").count(),
            2
        );
        assert!(writes.iter().all(|(member, _)| member != "halt"));
    }
    drop(first);
    drop(second);
    a.await.unwrap().unwrap();
    b.await.unwrap().unwrap();
    runtime.shutdown().await.unwrap();
}
