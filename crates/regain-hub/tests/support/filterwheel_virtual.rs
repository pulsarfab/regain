use super::*;
use regain_hub::{
    config::{DeviceType, HubConfig, OutputConfig, SourceBackend, SourceConfig, VirtualDevice},
    factory::NoCredentials,
    native::NativeRuntime,
    runtime::HubRuntime,
};

async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

pub(super) async fn nested(
    mut leaf: SourceConfig,
    device: Arc<Device>,
    connected: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<(String, String, Values)>>>,
    version: u16,
    lose_reply: bool,
    case: WheelTransportCase,
) {
    let mut config = HubConfig::empty();
    let leaf_id = leaf.id;
    leaf.polling.poll_seconds = if case == WheelTransportCase::Cache {
        0.1
    } else {
        300.0
    };
    leaf.polling.connection_timeout_seconds = 3.0;
    config.sources.push(leaf);
    let mut output = Uuid::new_v4();
    let leaf_output = output;
    config.outputs.push(OutputConfig {
        id: output,
        number: 4,
        label: "Private leaf wheel".into(),
        device: VirtualDevice::Proxy {
            source: leaf_id,
            device_type: DeviceType::FilterWheel,
        },
    });
    for number in [42, 91] {
        let source = Uuid::new_v4();
        config.sources.push(SourceConfig {
            id: source,
            label: "Nested wheel input".into(),
            backend: SourceBackend::Virtual { output },
            polling: PollPolicy {
                poll_seconds: 0.1,
                request_timeout_seconds: 0.1,
                connection_timeout_seconds: 4.0,
                ..Default::default()
            },
        });
        output = Uuid::new_v4();
        config.outputs.push(OutputConfig {
            id: output,
            number,
            label: "Nested wheel output".into(),
            device: VirtualDevice::Proxy {
                source,
                device_type: DeviceType::FilterWheel,
            },
        });
    }
    let outer_source = config.sources.last().unwrap().id;
    let directory = tempfile::tempdir().unwrap();
    let hub = HubRuntime::build(
        config,
        &NativeRuntime {
            cameras: None,
            directory: directory.path().into(),
            simulate: false,
            references: None,
        },
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    assert!(
        hub.source_snapshots()
            .iter()
            .all(|state| state.lease_count == 0)
    );
    assert!(hub.outputs().iter().all(|output| !output.simulated));
    let first = hub.client();
    let second = hub.client();
    device.pending.store(true, SeqCst);
    if case == WheelTransportCase::CancelNested {
        let pending = tokio::spawn({
            let client = first.clone();
            async move { client.connect(output).await }
        });
        until(|| {
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, member, _)| member == "interfaceversion")
        })
        .await;
        assert!(first.connection(output).is_err());
        pending.abort();
        assert!(pending.await.unwrap_err().is_cancelled());
        first.close();
        second.close();
        until(|| {
            hub.active_connections() == 0
                && hub
                    .source_snapshots()
                    .iter()
                    .all(|state| state.lease_count == 0)
        })
        .await;
        assert!(device.writes.lock().unwrap().is_empty());
        hub.shutdown().await.unwrap();
        assert!(!connected.load(SeqCst));
        return;
    }
    let at = tokio::time::Instant::now();
    first.connect(output).await.unwrap();
    second.connect(output).await.unwrap();
    assert!(at.elapsed() >= Duration::from_millis(700));
    device.pending.store(false, SeqCst);
    assert_eq!(
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, member, _)| member == "interfaceversion")
            .count(),
        1
    );
    let a = first.connection(output).unwrap();
    let b = second.connection(output).unwrap();
    let aw = a.filterwheel().unwrap();
    let bw = b.filterwheel().unwrap();
    assert_eq!(aw.generation(), bw.generation());
    assert_eq!(
        aw.property(FilterWheelProperty::Names).await.unwrap(),
        json!(["L", "Hα", ""])
    );
    assert_eq!(
        bw.property(FilterWheelProperty::FocusOffsets)
            .await
            .unwrap(),
        json!([-12, 0, 17])
    );
    let info = hub
        .source_snapshot(leaf_id)
        .unwrap()
        .connection_info
        .unwrap();
    assert_eq!(info.interface_version, Some(version));
    assert_eq!(
        info.method,
        if version == 3 {
            regain_hub::source::ConnectionMethod::Async
        } else {
            regain_hub::source::ConnectionMethod::Legacy
        }
    );
    until(|| {
        hub.source_snapshot(outer_source)
            .unwrap()
            .values
            .get("position")
            == Some(&json!(0))
    })
    .await;
    if case == WheelTransportCase::Cache {
        device.set("focusoffsets", json!([0]));
        until(|| {
            let state = hub.source_snapshot(outer_source).unwrap();
            state.sample_errors.len() == 3 && state.values.is_empty()
        })
        .await;
        assert!(device.writes.lock().unwrap().is_empty());
        assert_eq!(
            aw.position().await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        device.set("focusoffsets", json!([-12, 0, 17]));
        until(|| {
            hub.source_snapshot(outer_source)
                .unwrap()
                .sample_errors
                .is_empty()
        })
        .await;
        device.set("position", json!("0"));
        until(|| {
            hub.source_snapshots().iter().all(|state| {
                // The Alpaca sampler classifies a malformed wire value as
                // Permanent. Every virtual layer must retain that classification.
                state
                    .sample_errors
                    .get("position")
                    .is_some_and(|error| error.kind == ErrorKind::Permanent)
                    && state.values.get("names") == Some(&json!(["L", "Hα", ""]))
                    && state.values.get("focusoffsets") == Some(&json!([-12, 0, 17]))
                    && !state.values.contains_key("position")
            })
        })
        .await;
        assert_eq!(
            aw.position().await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert!(device.writes.lock().unwrap().is_empty());
        device.set("position", json!(0));
        until(|| {
            hub.source_snapshot(outer_source)
                .unwrap()
                .sample_errors
                .is_empty()
        })
        .await;
    } else {
        let sequence = hub.source_snapshot(leaf_id).unwrap().sequence;
        until(|| {
            hub.source_snapshot(outer_source)
                .unwrap()
                .sample_ages_seconds
                .get("names")
                .is_some_and(|age| *age > 0.15)
        })
        .await;
        let state = hub.source_snapshot(outer_source).unwrap();
        assert_eq!(state.values["names"], json!(["L", "Hα", ""]));
        assert_eq!(state.values["focusoffsets"], json!([-12, 0, 17]));
        assert_eq!(
            hub.source_snapshot(leaf_id).unwrap().sequence,
            sequence,
            "Nested cache polling performed leaf IO"
        );
    }
    if case == WheelTransportCase::LoseNestedGeneration {
        let old = aw.generation();
        *device.hang_read.lock().unwrap() = Some("position".into());
        assert!(aw.move_to(2).await.is_err());
        assert!(!aw.connected());
        assert!(device.writes.lock().unwrap().is_empty());
        *device.hang_read.lock().unwrap() = None;
        until(|| !hub.source_snapshot(leaf_id).unwrap().transport_connected).await;
        let fresh = hub.client();
        fresh.connect(output).await.unwrap();
        let fresh_connection = fresh.connection(output).unwrap();
        assert_ne!(fresh_connection.filterwheel().unwrap().generation(), old);
        let position = fresh_connection.filterwheel().unwrap().position().await;
        assert!(
            matches!(position, Ok(0)),
            "Fresh nested wheel read failed: {position:?}; sources={:?}; private requests={:?}; writes={:?}",
            hub.source_snapshots(),
            requests.lock().unwrap(),
            device.writes.lock().unwrap()
        );
        assert_eq!(
            aw.move_to(1).await.unwrap_err().kind,
            ErrorKind::Disconnected
        );
        assert!(device.writes.lock().unwrap().is_empty());
        assert!(
            hub.source_snapshots()
                .iter()
                .all(|state| !state.write_uncertain)
        );
        fresh.close();
        drop(fresh_connection);
    } else if lose_reply {
        assert_eq!(aw.move_to(2).await.unwrap_err().kind, ErrorKind::Uncertain);
        assert_eq!(bw.move_to(1).await.unwrap_err().kind, ErrorKind::Uncertain);
        assert!(!aw.connected());
        assert!(!bw.connected());
        assert_eq!(device.writes.lock().unwrap().len(), 1);
        assert!(hub.source_snapshot(leaf_id).unwrap().write_uncertain);
    } else {
        let direct = hub.client();
        direct.connect(leaf_output).await.unwrap();
        let direct_connection = direct.connection(leaf_output).unwrap();
        assert_eq!(
            aw.move_to(3).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert!(device.writes.lock().unwrap().is_empty());
        aw.move_to(2).await.unwrap();
        let started = std::time::Instant::now();
        let moving = bw.position().await;
        assert!(
            matches!(moving, Ok(-1)),
            "Nested moving-position read failed after {:?}: {moving:?}; sources={:?}; private requests={:?}; writes={:?}",
            started.elapsed(),
            hub.source_snapshots(),
            requests.lock().unwrap(),
            device.writes.lock().unwrap()
        );
        assert_eq!(
            direct_connection
                .filterwheel()
                .unwrap()
                .position()
                .await
                .unwrap(),
            -1
        );
        assert_eq!(bw.move_to(1).await.unwrap_err().kind, ErrorKind::Busy);
        device.set("position", json!(1));
        assert_eq!(bw.position().await.unwrap(), 1);
        device.set("position", json!(2));
        assert_eq!(aw.position().await.unwrap(), 2);
        first.disconnect(output);
        drop(a);
        assert!(bw.connected());
        assert_eq!(bw.position().await.unwrap(), 2);
        direct.close();
        drop(direct_connection);
        second.close();
        drop(b);
        hub.shutdown().await.unwrap();
        assert!(!connected.load(SeqCst));
        assert!(
            hub.source_snapshots()
                .iter()
                .all(|state| state.lease_count == 0)
        );
        assert_eq!(device.writes.lock().unwrap().len(), 1);
        return;
    }
    first.close();
    second.close();
    drop(a);
    drop(b);
    hub.shutdown().await.unwrap();
    assert!(
        hub.source_snapshots()
            .iter()
            .all(|state| state.lease_count == 0)
    );
    assert!(!connected.load(SeqCst));
}

#[tokio::test]
async fn nested_wheel_v2_preserves_arrays_motion_ages_and_ownership() {
    alpaca_wheel(2, false, WheelTransportCase::Nested).await;
}
#[tokio::test]
async fn nested_wheel_v3_preserves_arrays_motion_ages_and_ownership() {
    alpaca_wheel(3, false, WheelTransportCase::Nested).await;
}
#[tokio::test]
async fn nested_wheel_unknown_position_reply_is_not_replayed() {
    alpaca_wheel(3, true, WheelTransportCase::Nested).await;
}
#[tokio::test]
async fn cancelled_nested_wheel_connect_releases_supervised_inner_clients() {
    alpaca_wheel(3, false, WheelTransportCase::CancelNested).await;
}
#[tokio::test]
async fn nested_wheel_generation_loss_cannot_rebind_old_sessions_or_move() {
    alpaca_wheel(3, false, WheelTransportCase::LoseNestedGeneration).await;
}
#[tokio::test]
async fn nested_wheel_cache_keeps_array_dependencies_and_per_key_recovery() {
    alpaca_wheel(3, false, WheelTransportCase::Cache).await;
}
