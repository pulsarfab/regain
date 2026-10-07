use super::*;
use regain_hub::{
    config::{HubConfig, OutputConfig, SourceBackend, SourceConfig, VirtualDevice},
    factory::NoCredentials,
    native::NativeRuntime,
    runtime::HubRuntime,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Case {
    Normal,
    Cache,
    CancelConnection,
    LoseGeneration,
}

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
    case: Case,
) {
    let leaf_id = leaf.id;
    leaf.polling.poll_seconds = if case == Case::Cache { 0.1 } else { 300.0 };
    let mut config = HubConfig::empty();
    config.sources.push(leaf);
    let mut output = Uuid::new_v4();
    let leaf_output = output;
    config.outputs.push(OutputConfig {
        id: output,
        number: 4,
        label: "Private leaf panel".into(),
        device: VirtualDevice::Proxy {
            source: leaf_id,
            device_type: DeviceType::CoverCalibrator,
        },
    });
    for number in [42, 91] {
        let source = Uuid::new_v4();
        config.sources.push(SourceConfig {
            id: source,
            label: "Nested panel input".into(),
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
            label: "Nested panel output".into(),
            device: VirtualDevice::Proxy {
                source,
                device_type: DeviceType::CoverCalibrator,
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
    assert!(hub.source_snapshots().iter().all(|s| s.lease_count == 0));
    assert!(hub.outputs().iter().all(|o| !o.simulated));
    let first = hub.client();
    let second = hub.client();
    device.pending.store(true, SeqCst);
    if case == Case::CancelConnection {
        let task = tokio::spawn({
            let first = first.clone();
            async move { first.connect(output).await }
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
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        first.close();
        second.close();
        until(|| {
            hub.active_connections() == 0
                && hub.source_snapshots().iter().all(|s| s.lease_count == 0)
        })
        .await;
        hub.shutdown().await.unwrap();
        assert!(device.writes.lock().unwrap().is_empty());
        assert!(!connected.load(SeqCst));
        return;
    }
    let at = tokio::time::Instant::now();
    first.connect(output).await.unwrap();
    second.connect(output).await.unwrap();
    assert!(at.elapsed() >= Duration::from_millis(700));
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
    let ap = a.covercalibrator().unwrap();
    let bp = b.covercalibrator().unwrap();
    assert_eq!(ap.generation(), bp.generation());
    assert_eq!(
        hub.source_snapshot(leaf_id)
            .unwrap()
            .connection_info
            .unwrap()
            .interface_version,
        Some(version)
    );
    until(|| {
        hub.source_snapshot(outer_source)
            .unwrap()
            .values
            .get("maxbrightness")
            == Some(&json!(4096))
            && hub.source_snapshot(leaf_id).unwrap().completed_passes > 0
    })
    .await;
    if case == Case::Cache {
        device.set("maxbrightness", json!(0));
        until(|| {
            hub.source_snapshot(leaf_id)
                .unwrap()
                .values
                .get("maxbrightness")
                == Some(&json!(0))
                && hub
                    .source_snapshots()
                    .iter()
                    .filter(|s| s.source != leaf_id)
                    .all(|s| s.sample_errors.contains_key("maxbrightness"))
                && hub
                    .source_snapshot(outer_source)
                    .unwrap()
                    .sample_errors
                    .contains_key("brightness")
        })
        .await;
        assert_eq!(
            ap.calibrator_on(1).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert!(device.writes.lock().unwrap().is_empty());
        device.set("maxbrightness", json!(4096));
        until(|| {
            hub.source_snapshots()
                .iter()
                .all(|s| s.sample_errors.is_empty())
        })
        .await;
        device.set("covermoving", json!("false"));
        until(|| {
            hub.source_snapshots().iter().all(|s| {
                s.sample_errors
                    .get("covermoving")
                    .is_some_and(|e| e.kind == ErrorKind::Permanent)
                    && !s.values.contains_key("covermoving")
            })
        })
        .await;
        assert_eq!(
            ap.property(Property::CoverMoving).await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(
            ap.property(Property::CalibratorState).await.unwrap(),
            json!(1)
        );
        device.set("covermoving", json!(false));
        until(|| {
            hub.source_snapshots()
                .iter()
                .all(|s| s.sample_errors.is_empty())
        })
        .await;
        device.errors.lock().unwrap().insert(
            "coverstate".into(),
            SourceError::new(ErrorKind::Unavailable, "Private unavailable"),
        );
        until(|| {
            hub.source_snapshots().iter().all(|s| {
                s.sample_errors
                    .get("coverstate")
                    .is_some_and(|e| e.kind == ErrorKind::Unavailable)
            })
        })
        .await;
        assert_eq!(
            ap.property(Property::CalibratorState).await.unwrap(),
            json!(1)
        );
        device.errors.lock().unwrap().clear();
        until(|| {
            hub.source_snapshots()
                .iter()
                .all(|s| s.sample_errors.is_empty())
        })
        .await;
    } else {
        let sequence = hub.source_snapshot(leaf_id).unwrap().sequence;
        let reads = requests.lock().unwrap().len();
        let age = hub
            .source_snapshot(outer_source)
            .unwrap()
            .sample_ages_seconds
            .get("brightness")
            .copied()
            .unwrap();
        until(|| {
            hub.source_snapshot(outer_source)
                .unwrap()
                .sample_ages_seconds
                .get("brightness")
                .is_some_and(|value| *value >= age + 0.15)
        })
        .await;
        assert_eq!(hub.source_snapshot(leaf_id).unwrap().sequence, sequence);
        assert_eq!(
            requests.lock().unwrap().len(),
            reads,
            "Virtual cache polling performed leaf I/O"
        );
    }
    if case == Case::LoseGeneration {
        let generation = ap.generation();
        *device.hang_read.lock().unwrap() = Some("coverstate".into());
        assert!(ap.open_cover().await.is_err());
        assert!(!ap.connected());
        assert!(device.writes.lock().unwrap().is_empty());
        *device.hang_read.lock().unwrap() = None;
        until(|| !hub.source_snapshot(leaf_id).unwrap().transport_connected).await;
        let fresh = hub.client();
        fresh.connect(output).await.unwrap();
        let connection = fresh.connection(output).unwrap();
        assert_ne!(
            connection.covercalibrator().unwrap().generation(),
            generation
        );
        assert_eq!(
            connection
                .covercalibrator()
                .unwrap()
                .property(Property::CoverState)
                .await
                .unwrap(),
            json!(1)
        );
        assert_eq!(
            ap.open_cover().await.unwrap_err().kind,
            ErrorKind::Disconnected
        );
        assert!(device.writes.lock().unwrap().is_empty());
        assert!(hub.source_snapshots().iter().all(|s| !s.write_uncertain));
        fresh.close();
        drop(connection);
    } else if lose_reply {
        assert_eq!(
            ap.calibrator_on(17).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(device.values.lock().unwrap()["brightness"], json!(17));
        assert_eq!(
            bp.calibrator_off().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            bp.open_cover().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            bp.close_cover().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            bp.halt_cover().await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert!(!bp.connected());
        assert!(hub.source_snapshot(outer_source).unwrap().write_uncertain);
        // Retired inner transports can release their last lease and become idle.
        // Every source still owned by a client must retain the command fence.
        assert!(
            hub.source_snapshots()
                .iter()
                .all(|s| s.lease_count == 0 || s.write_uncertain),
            "{}",
            serde_json::to_string(&hub.source_snapshots()).unwrap()
        );
        assert_eq!(device.writes.lock().unwrap().len(), 1);
    } else {
        let direct = hub.client();
        direct.connect(leaf_output).await.unwrap();
        let connection = direct.connection(leaf_output).unwrap();
        assert_eq!(
            ap.calibrator_on(4097).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert!(device.writes.lock().unwrap().is_empty());
        ap.open_cover().await.unwrap();
        assert_eq!(
            bp.property(Property::CoverMoving).await.unwrap(),
            json!(true)
        );
        assert_eq!(
            connection
                .covercalibrator()
                .unwrap()
                .property(Property::CoverState)
                .await
                .unwrap(),
            json!(2)
        );
        bp.halt_cover().await.unwrap();
        assert_eq!(ap.property(Property::CoverState).await.unwrap(), json!(4));
        if version == 1 {
            assert_eq!(
                ap.property(Property::CoverMoving).await.unwrap_err().kind,
                ErrorKind::Unavailable
            );
        } else {
            assert_eq!(
                ap.property(Property::CoverMoving).await.unwrap(),
                json!(false)
            );
        }
        ap.calibrator_on(0).await.unwrap();
        assert_eq!(bp.property(Property::Brightness).await.unwrap(), json!(0));
        assert_eq!(
            bp.property(Property::CalibratorChanging).await.unwrap(),
            json!(true)
        );
        first.disconnect(output);
        drop(a);
        assert!(bp.connected());
        assert_eq!(
            bp.property(Property::CalibratorState).await.unwrap(),
            json!(2)
        );
        direct.close();
        drop(connection);
        second.close();
        drop(b);
        hub.shutdown().await.unwrap();
        assert!(!connected.load(SeqCst));
        assert!(hub.source_snapshots().iter().all(|s| s.lease_count == 0));
        assert_eq!(device.writes.lock().unwrap().len(), 3);
        return;
    }
    first.close();
    second.close();
    drop(a);
    drop(b);
    hub.shutdown().await.unwrap();
    assert!(hub.source_snapshots().iter().all(|s| s.lease_count == 0));
    assert!(!connected.load(SeqCst));
}

#[tokio::test]
async fn nested_legacy_panel_preserves_ownership_unknown_completion_and_cache_age() {
    alpaca_panel_case(1, false, Some(Case::Normal)).await;
}
#[tokio::test]
async fn nested_modern_panel_preserves_independent_completion_and_cache_age() {
    alpaca_panel_case(2, false, Some(Case::Normal)).await;
}
#[tokio::test]
async fn nested_panel_preserves_dependency_errors_and_partial_cache_recovery() {
    alpaca_panel_case(2, false, Some(Case::Cache)).await;
}
#[tokio::test]
async fn nested_panel_applied_lost_ack_fences_active_layers_without_actuator_cleanup() {
    alpaca_panel_case(2, true, Some(Case::Normal)).await;
}
#[tokio::test]
async fn cancelled_nested_panel_connection_releases_every_layer_without_actuation() {
    alpaca_panel_case(2, false, Some(Case::CancelConnection)).await;
}
#[tokio::test]
async fn nested_panel_lost_preflight_generation_cannot_retarget_old_sessions() {
    alpaca_panel_case(2, false, Some(Case::LoseGeneration)).await;
}
