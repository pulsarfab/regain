//! Actual native factory/actors with explicit SDK/direct production simulations.
use regain_hub::{
    camera::{
        acquisition::{AcquisitionTiming, CameraSupervisor, ExposureRequest},
        config::{CameraRecovery, NativeCameraConfig},
        image::{ElementType, ImageDescriptor, ImageOrder},
        properties::CameraSetting,
        runtime::{CameraResources, NativeCameraRuntime},
    },
    config::{HubConfig, NativeDevice, OutputConfig, SourceBackend, SourceConfig, VirtualDevice},
    factory::{NoCredentials, build_sources, source_plans},
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    source::ErrorKind,
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

type Events = Arc<Mutex<Vec<String>>>;
fn native(resources: CameraResources) -> (NativeRuntime, Events) {
    let events: Events = Arc::default();
    let output = events.clone();
    (
        NativeRuntime {
            directory: std::env::var_os("REGAIN_TEST_WORKERS")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
                }),
            simulate: true,
            references: None,
            cameras: Some(NativeCameraRuntime {
                sdk: "unused".into(),
                sdk_simulation: Some(json!({"instant":true})),
                resources,
                diagnostic: Arc::new(move |_, event, message| {
                    output.lock().unwrap().push(format!("{event}: {message}"))
                }),
            }),
        },
        events,
    )
}
fn config(direct: bool) -> HubConfig {
    let mut config = HubConfig::empty();
    config.sources.push(SourceConfig {
        id: Uuid::new_v4(),
        label: "Explicit camera simulation".into(),
        backend: SourceBackend::Native {
            device: if direct {
                NativeDevice::CameraDirect
            } else {
                NativeDevice::CameraSdk
            },
            identity: if direct {
                "direct-simulator"
            } else {
                "sim00001"
            }
            .into(),
            filter_wheel: None,
            camera: Some(NativeCameraConfig {
                model: if direct {
                    "ZWO ASI585MM Pro"
                } else {
                    "ZWO Simulated"
                }
                .into(),
                sdk_fallback: false,
                recovery: CameraRecovery(regain_core::RecoveryOptions {
                    max_retries: 0,
                    ready_frame_download_retries: 0,
                    reconnect_delay_seconds: 0.01,
                    ..Default::default()
                }),
            }),
        },
        polling: regain_hub::parameters::PollPolicy {
            request_timeout_seconds: 5.,
            connection_timeout_seconds: 10.,
            poll_seconds: 60.,
            ..Default::default()
        },
    });
    config
}
async fn until(mut test: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn supervisor(
    source: Arc<regain_hub::source::SourceHandle>,
    resources: &CameraResources,
) -> Arc<CameraSupervisor> {
    CameraSupervisor::new(
        source,
        resources.image_budget(),
        AcquisitionTiming {
            poll_interval: Duration::from_millis(10),
            ..Default::default()
        },
        resources.activity(),
    )
    .unwrap()
}
async fn geometry(session: &regain_hub::camera::acquisition::CameraSession) {
    session.set(CameraSetting::NumX(64)).await.unwrap();
    session.set(CameraSetting::NumY(64)).await.unwrap();
}
fn request() -> ExposureRequest {
    ExposureRequest {
        duration_seconds: 0.01,
        light: false,
    }
}

#[tokio::test]
async fn native_camera_factory_requires_host_resources_and_is_inert_with_missing_workers() {
    let resources = CameraResources::default();
    let (mut native, events) = native(resources.clone());
    let temp = tempfile::tempdir().unwrap();
    native.directory = temp.path().join("missing-workers");
    native.simulate = false;
    let settings = native.cameras.as_mut().unwrap();
    settings.sdk_simulation = None;
    settings.sdk = temp.path().join("missing-sdk");
    let config = config(false);
    let registry = build_sources(
        &config,
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    tokio::task::yield_now().await;
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(resources.activity().active(), 0);
    assert_eq!(resources.image_budget().used_bytes(), 0);
    let state = registry.get(config.sources[0].id).unwrap().snapshot();
    assert!(!state.transport_connected && !state.simulated);
    assert_eq!(state.lease_count, 0);
    registry.shutdown().await.unwrap();
    native.cameras = None;
    assert!(
        build_sources(
            &config,
            &native,
            &NoCredentials,
            Arc::new(MonotonicClock::default())
        )
        .is_err()
    );
}

#[tokio::test]
async fn camera_poll_plans_deduplicate_typed_properties_with_gated_frontend_choices() {
    let mut cfg = config(false);
    let source = cfg.sources[0].id;
    for number in [2, 9] {
        cfg.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: "Gated camera".into(),
            device: VirtualDevice::Proxy {
                source,
                device_type: regain_hub::config::DeviceType::Camera,
            },
        });
    }
    let plans = source_plans(&cfg).unwrap();
    assert_eq!(
        plans[&source].samples.len(),
        regain_hub::camera::properties::CameraProperty::ALL.len()
    );
    assert_eq!(plans[&source].source.polling, cfg.sources[0].polling);
    let (native, _) = native(CameraResources::default());
    let runtime = HubRuntime::build(
        cfg,
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    assert_eq!(runtime.outputs().len(), 2);
    assert_eq!(runtime.source_snapshot(source).unwrap().lease_count, 0);
    assert!(!runtime.source_snapshot(source).unwrap().transport_connected);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_factory_sdk_and_direct_share_capture_memory_and_preserve_pinned_readers_across_revisions()
 {
    for direct in [false, true] {
        const CAPACITY: usize = 512 * 1024;
        let resources = CameraResources::new(CAPACITY).unwrap();
        let budget = resources.image_budget();
        let (mut native, events) = native(resources.clone());
        // Keep SDK work alive long enough to observe retained host activity;
        // instant captures could finish before the assertion's scheduling turn.
        native.cameras.as_mut().unwrap().sdk_simulation = Some(json!({"instant":false}));
        let cfg = config(direct);
        let clock = Arc::new(MonotonicClock::default());
        // A host runtime must observe the same retained work even when it has no
        // connected output. Resources belong to the builder, not one revision.
        let observer =
            HubRuntime::build(HubConfig::empty(), &native, &NoCredentials, clock.clone()).unwrap();
        let registry = build_sources(&cfg, &native, &NoCredentials, clock.clone()).unwrap();
        let source = registry.get(cfg.sources[0].id).unwrap();
        let owner = supervisor(source.clone(), &resources);
        let first = owner.connect().await.unwrap();
        let sibling = owner.connect().await.unwrap();
        geometry(&first).await;
        first
            .start(ExposureRequest {
                duration_seconds: 0.2,
                light: false,
            })
            .await
            .unwrap();
        assert!(observer.active_connections() > 0);
        until(|| owner.status().image_ready).await;
        let pinned = first.image().unwrap();
        assert!(Arc::ptr_eq(&pinned, &sibling.image().unwrap()));
        assert!(source.snapshot().simulated);
        let old_charge = budget.used_bytes();
        assert!(old_charge > 8192);
        drop(first);
        drop(sibling);
        registry.shutdown().await.unwrap();
        drop(owner);
        drop(registry);
        until(|| resources.activity().active() == 0).await;
        assert_eq!(budget.used_bytes(), old_charge);
        let original = pinned.image.bytes().to_vec();
        let mut next = cfg.clone();
        next.revision = Uuid::new_v4();
        let registry = build_sources(&next, &native.clone(), &NoCredentials, clock).unwrap();
        let owner = supervisor(registry.get(next.sources[0].id).unwrap(), &resources);
        let second = owner.connect().await.unwrap();
        geometry(&second).await;
        let fill = budget
            .allocate(
                ImageDescriptor::new(
                    (CAPACITY - old_charge) as u32,
                    1,
                    None,
                    ElementType::Byte,
                    ElementType::Byte,
                    ImageOrder::SensorRows,
                )
                .unwrap(),
            )
            .unwrap();
        let before = events
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.contains("Starting exposure"))
            .count();
        assert_eq!(
            second.start(request()).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.contains("Starting exposure"))
                .count(),
            before
        );
        assert_eq!(pinned.image.bytes(), original);
        drop(fill);
        second.start(request()).await.unwrap();
        until(|| owner.status().image_ready).await;
        assert!(budget.used_bytes() > old_charge);
        assert_eq!(pinned.image.bytes(), original);
        drop(second);
        registry.shutdown().await.unwrap();
        drop(owner);
        drop(registry);
        until(|| resources.activity().active() == 0).await;
        assert_eq!(budget.used_bytes(), old_charge);
        drop(pinned);
        assert_eq!(budget.used_bytes(), 0);
        assert_eq!(observer.active_connections(), 0);
        observer.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn native_factory_never_enables_simulation_from_a_fixture_or_missing_sdk() {
    let (mut native, _) = native(CameraResources::default());
    native.simulate = false;
    let cfg = config(false);
    assert!(
        build_sources(
            &cfg,
            &native,
            &NoCredentials,
            Arc::new(MonotonicClock::default())
        )
        .is_err()
    );
    native.cameras.as_mut().unwrap().sdk_simulation = None;
    assert!(
        build_sources(
            &cfg,
            &native,
            &NoCredentials,
            Arc::new(MonotonicClock::default())
        )
        .is_err()
    );
}
