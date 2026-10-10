//! Real source factory, nested controllers and image accounting; no SDK/hardware.
use regain_hub::{
    camera::{
        acquisition::{
            AcquisitionPhase, CameraSession, CapturedImage, ExposureRequest, GuideRequest,
        },
        image::{ElementType, ImageOrder},
        properties::{CameraProperty as P, CameraSetting as S, CameraValue as V},
        runtime::CameraResources,
    },
    config::HubConfig,
    factory::NoCredentials,
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    source::ErrorKind,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

const FRAME: usize = 320 * 240 * 2;
struct Fixture {
    runtime: Arc<HubRuntime>,
    resources: CameraResources,
    sources: Vec<Uuid>,
    outputs: Vec<Uuid>,
}
impl Fixture {
    fn new(maximum: usize) -> Self {
        Self::with_native(maximum, None)
    }
    fn with_native(maximum: usize, direct: Option<bool>) -> Self {
        let mut config = HubConfig::empty();
        let mut sources = Vec::new();
        let mut outputs = Vec::new();
        for level in 0..3 {
            let source = Uuid::new_v4();
            let backend = if level == 0 {
                if let Some(direct) = direct {
                    json!({"kind":"native","device":if direct {"camera-direct"} else {"camera-sdk"},
                        "identity":if direct {"direct-simulator"} else {"sim00001"},
                        "camera":{"model":if direct {"ZWO ASI585MM Pro"} else {"ZWO Simulated"},
                            "recovery":{"maxRetries":0,"readyFrameDownloadRetries":0,"reconnectDelaySeconds":0.01}}})
                } else {
                    json!({"kind":"simulated","deviceType":"camera"})
                }
            } else {
                json!({"kind":"virtual","output":outputs[level - 1]})
            };
            config.sources.push(serde_json::from_value(json!({
                "id":source,"label":"Private nested camera input","backend":backend,
                "polling":{"pollSeconds":0.1,"requestTimeoutSeconds":2,"connectionTimeoutSeconds":10}
            })).unwrap());
            let output = Uuid::new_v4();
            config.outputs.push(
                serde_json::from_value(json!({
                    "id":output,"number":level * 3,"label":"Private nested camera output",
                    "device":{"kind":"proxy","source":source,"deviceType":"camera"}
                }))
                .unwrap(),
            );
            sources.push(source);
            outputs.push(output);
        }
        let sibling = Uuid::new_v4();
        config.outputs.push(
            serde_json::from_value(json!({
                "id":sibling,"number":10,"label":"Private camera sibling",
                "device":{"kind":"proxy","source":sources[2],"deviceType":"camera"}
            }))
            .unwrap(),
        );
        outputs.push(sibling);
        let resources = CameraResources::new(maximum).unwrap();
        let runtime = HubRuntime::build_with_camera_resources(
            config,
            &NativeRuntime {
                directory: std::env::var_os("REGAIN_TEST_WORKERS")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| {
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
                    }),
                simulate: direct.is_some(),
                references: None,
                cameras: direct.map(|_| regain_hub::camera::runtime::NativeCameraRuntime {
                    sdk: "unused".into(),
                    sdk_simulation: Some(json!({"instant":false})),
                    resources: resources.clone(),
                    diagnostic: Arc::new(|_, _, _| {}),
                }),
            },
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
            resources.clone(),
        )
        .unwrap();
        Self {
            runtime,
            resources,
            sources,
            outputs,
        }
    }
    async fn finish(&self) {
        self.runtime.shutdown().await.unwrap();
        assert!(
            self.runtime
                .source_snapshots()
                .iter()
                .all(|source| source.lease_count == 0)
        );
    }
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn capture(camera: &CameraSession, seconds: f64) -> Arc<CapturedImage> {
    let id = camera
        .start(ExposureRequest {
            duration_seconds: seconds,
            light: true,
        })
        .await
        .unwrap();
    until(|| {
        camera
            .status()
            .completed
            .is_some_and(|image| image.acquisition == id)
    })
    .await;
    camera.image().unwrap()
}

#[tokio::test(start_paused = true)]
async fn nested_guiding_and_exposure_share_each_layers_control_and_disconnect_does_not_abort() {
    let f = Fixture::new(FRAME);
    f.runtime
        .update_simulation(
            f.sources[0],
            serde_json::from_value(json!({"camera":{"canPulseGuide":true}})).unwrap(),
        )
        .await
        .unwrap();
    let owner = f.runtime.client();
    let observer = f.runtime.client();
    owner.connect(f.outputs[2]).await.unwrap();
    observer.connect(f.outputs[3]).await.unwrap();
    let a = owner.connection(f.outputs[2]).unwrap();
    let b = observer.connection(f.outputs[3]).unwrap();
    let camera = a.camera().unwrap();
    let sibling = b.camera().unwrap();
    let connected_activity = f.resources.activity().active();
    camera
        .pulse_guide(GuideRequest {
            direction: 1,
            duration_milliseconds: 5000,
        })
        .await
        .unwrap();
    let image = capture(camera, 0.01).await;
    assert_eq!(image.image.bytes().len(), FRAME);
    assert!(camera.status().guiding.is_some());
    assert_eq!(
        sibling
            .pulse_guide(GuideRequest {
                direction: 0,
                duration_milliseconds: 0
            })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    assert_eq!(
        camera.set(S::Gain(1)).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    owner.close();
    drop(a);
    assert!(f.resources.activity().active() > 0);
    until(|| sibling.status().guiding.is_none()).await;
    sibling.set(S::Gain(1)).await.unwrap();
    // The observer and two internal virtual connections still own activity.
    assert_eq!(f.resources.activity().active(), connected_activity - 1);
    observer.close();
    drop(b);
    drop(image);
    f.finish().await;
    assert_eq!(f.resources.activity().active(), 0);
}

#[tokio::test(start_paused = true)]
async fn nested_camera_capture_shares_one_immutable_budget_reservation_and_keeps_each_identity() {
    let f = Fixture::new(FRAME);
    assert!(
        f.runtime
            .source_snapshots()
            .iter()
            .all(|source| source.lease_count == 0)
    );
    let client = f.runtime.client();
    client.connect(f.outputs[2]).await.unwrap();
    let connection = client.connection(f.outputs[2]).unwrap();
    let camera = connection.camera().unwrap();
    let image = capture(camera, 0.1).await;
    assert_eq!(image.identity.source, f.sources[2]);
    assert_eq!(image.image.descriptor().order(), ImageOrder::SensorRows);
    assert_eq!(image.image.descriptor().element_type(), ElementType::Int32);
    assert_eq!(
        image.image.descriptor().transmission_type(),
        ElementType::UInt16
    );
    assert_eq!(f.resources.image_budget().used_bytes(), FRAME);
    let observer = f.runtime.client();
    for (index, source) in f.sources.iter().enumerate() {
        observer.connect(f.outputs[index]).await.unwrap();
        let inner = observer.connection(f.outputs[index]).unwrap();
        let frame = inner.camera().unwrap().image().unwrap();
        assert_eq!(frame.identity.source, *source);
        if index != 2 {
            assert_ne!(frame.identity.acquisition, image.identity.acquisition);
        }
        assert_eq!(frame.image.bytes().as_ptr(), image.image.bytes().as_ptr());
        assert_eq!(
            frame.identity.exposure.start_time,
            image.identity.exposure.start_time
        );
        assert_eq!(frame.identity.exposure.duration_seconds, Some(0.1));
    }
    client.close();
    observer.close();
    drop(connection);
    f.finish().await;
    assert_eq!(f.resources.image_budget().used_bytes(), FRAME);
    assert_eq!(image.image.bytes().len(), FRAME);
    drop(image);
    assert_eq!(f.resources.image_budget().used_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn virtual_camera_settings_and_all_image_ranks_use_common_types() {
    let f = Fixture::new(FRAME * 8);
    let client = f.runtime.client();
    client.connect(f.outputs[2]).await.unwrap();
    let connection = client.connection(f.outputs[2]).unwrap();
    let camera = connection.camera().unwrap();
    for setting in [
        S::BinX(2),
        S::BinY(3),
        S::NumX(4),
        S::NumY(2),
        S::StartX(2),
        S::StartY(3),
        S::Gain(12),
        S::Offset(5),
        S::CoolerOn(true),
        S::SetCcdTemperature(-15.0),
    ] {
        camera.set(setting).await.unwrap();
    }
    assert_eq!(
        camera.property(P::CoolerOn).await.unwrap(),
        V::Boolean { value: true }
    );
    assert_eq!(
        camera.property(P::SetCcdTemperature).await.unwrap(),
        V::Number { value: -15.0 }
    );
    for (mode, planes) in [(0, None), (1, Some(3)), (2, Some(1))] {
        camera.set(S::ReadoutMode(mode)).await.unwrap();
        let image = capture(camera, 0.1).await;
        assert_eq!(
            (
                image.image.descriptor().width(),
                image.image.descriptor().height()
            ),
            (4, 2)
        );
        assert_eq!(image.image.descriptor().planes(), planes);
        assert_eq!(
            (image.identity.geometry.bin_x, image.identity.geometry.bin_y),
            (2, 3)
        );
        assert_eq!(
            image.image.bytes().len(),
            4 * 2 * planes.unwrap_or(1) as usize * 2
        );
    }
    assert_eq!(
        camera.property(P::Gains).await.unwrap_err().kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        camera.set(S::NumX(0)).await.unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    client.close();
    drop(connection);
    f.finish().await;
    assert_eq!(f.resources.image_budget().used_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn virtual_camera_stop_is_partial_and_abort_discards_without_affecting_siblings() {
    let f = Fixture::new(FRAME * 4);
    let owner = f.runtime.client();
    let observer = f.runtime.client();
    owner.connect(f.outputs[2]).await.unwrap();
    observer.connect(f.outputs[3]).await.unwrap();
    let connection = owner.connection(f.outputs[2]).unwrap();
    let sibling = observer.connection(f.outputs[3]).unwrap();
    let camera = connection.camera().unwrap();
    camera
        .start(ExposureRequest {
            duration_seconds: 20.0,
            light: true,
        })
        .await
        .unwrap();
    assert_eq!(
        sibling.camera().unwrap().abort().await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        sibling
            .camera()
            .unwrap()
            .start(ExposureRequest {
                duration_seconds: 0.1,
                light: true
            })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    tokio::time::advance(Duration::from_secs(2)).await;
    camera.stop().await.unwrap();
    until(|| camera.status().image_ready).await;
    let image = camera.image().unwrap();
    let duration = image.identity.exposure.duration_seconds.unwrap();
    assert!(
        duration > 0.0 && duration < 20.0,
        "Stop lost partial integration: {duration}"
    );
    assert_eq!(
        sibling
            .camera()
            .unwrap()
            .image()
            .unwrap()
            .image
            .bytes()
            .as_ptr(),
        image.image.bytes().as_ptr()
    );
    drop(image);
    camera
        .start(ExposureRequest {
            duration_seconds: 20.0,
            light: false,
        })
        .await
        .unwrap();
    camera.abort().await.unwrap();
    assert_eq!(camera.status().phase, AcquisitionPhase::Idle);
    assert!(!camera.status().image_ready);
    assert!(camera.image().is_err());
    assert!(sibling.connected());
    owner.close();
    observer.close();
    drop(connection);
    drop(sibling);
    f.finish().await;
    assert_eq!(f.resources.image_budget().used_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn virtual_camera_old_pixels_and_metadata_survive_an_independent_inner_capture() {
    let f = Fixture::new(FRAME * 4);
    let owner = f.runtime.client();
    let inner = f.runtime.client();
    owner.connect(f.outputs[2]).await.unwrap();
    inner.connect(f.outputs[0]).await.unwrap();
    let outer = owner.connection(f.outputs[2]).unwrap();
    let direct = inner.connection(f.outputs[0]).unwrap();
    let image = capture(outer.camera().unwrap(), 0.1).await;
    let original = image.image.bytes().to_vec();
    let newer = capture(direct.camera().unwrap(), 0.2).await;
    assert_ne!(
        newer.identity.exposure.start_time,
        image.identity.exposure.start_time
    );
    assert_eq!(
        outer
            .camera()
            .unwrap()
            .property(P::LastExposureDuration)
            .await
            .unwrap(),
        V::Number { value: 0.1 }
    );
    assert_eq!(
        outer.camera().unwrap().image().unwrap().image.bytes(),
        original
    );
    assert_eq!(f.resources.image_budget().used_bytes(), FRAME * 2);
    drop(newer);
    owner.close();
    inner.close();
    drop(outer);
    drop(direct);
    f.finish().await;
    assert_eq!(f.resources.image_budget().used_bytes(), FRAME);
    drop(image);
    assert_eq!(f.resources.image_budget().used_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn virtual_camera_polls_preserve_original_observation_age_and_optional_errors() {
    let f = Fixture::new(FRAME * 4);
    f.runtime.update_simulation(f.sources[0], serde_json::from_value(json!({"sampleAgeSeconds":40,"camera":{"temperature":-12.0,"temperatureAvailable":false}})).unwrap()).await.unwrap();
    let client = f.runtime.client();
    client.connect(f.outputs[2]).await.unwrap();
    let outer = client.connection(f.outputs[2]).unwrap();
    assert_eq!(
        outer
            .camera()
            .unwrap()
            .property(P::CcdTemperature)
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    until(|| {
        f.runtime
            .source_snapshot(f.sources[2])
            .unwrap()
            .sample_errors
            .get("ccdtemperature")
            .is_some_and(|error| error.kind == ErrorKind::Unsupported)
    })
    .await;
    until(|| {
        f.runtime
            .source_snapshot(f.sources[2])
            .unwrap()
            .sample_ages_seconds
            .get("cameraxsize")
            .is_some_and(|age| *age >= 40.0)
    })
    .await;
    assert_eq!(
        f.runtime.source_snapshot(f.sources[2]).unwrap().values["cameraxsize"],
        320
    );
    client.close();
    drop(outer);
    f.finish().await;
}

#[tokio::test(start_paused = true)]
async fn losing_virtual_clients_keeps_dispatched_acquisition_alive_without_implicit_abort() {
    let f = Fixture::new(FRAME * 4);
    let owner = f.runtime.client();
    owner.connect(f.outputs[2]).await.unwrap();
    let outer = owner.connection(f.outputs[2]).unwrap();
    let id = outer
        .camera()
        .unwrap()
        .start(ExposureRequest {
            duration_seconds: 5.0,
            light: true,
        })
        .await
        .unwrap();
    owner.close();
    drop(outer);
    tokio::time::advance(Duration::from_secs(6)).await;
    let observer = f.runtime.client();
    observer.connect(f.outputs[3]).await.unwrap();
    let sibling = observer.connection(f.outputs[3]).unwrap();
    until(|| {
        sibling
            .camera()
            .unwrap()
            .status()
            .completed
            .is_some_and(|image| image.acquisition == id)
    })
    .await;
    assert_eq!(
        sibling
            .camera()
            .unwrap()
            .image()
            .unwrap()
            .identity
            .exposure
            .duration_seconds,
        Some(5.0)
    );
    observer.close();
    drop(sibling);
    f.finish().await;
    assert_eq!(f.resources.image_budget().used_bytes(), 0);
}

#[tokio::test]
async fn sdk_and_direct_native_images_keep_recovery_evidence_without_granting_proxy_retries() {
    use regain_hub::{
        client::{Client, ClientLimits},
        ipc::{Limits, serve_stream},
    };
    for direct in [false, true] {
        let f = Fixture::with_native(4 * 1024 * 1024, Some(direct));
        let (stream, server) = tokio::io::duplex(8192);
        let serving = tokio::spawn(serve_stream(server, f.runtime.clone(), Limits::default()));
        let timing = Client::from_stream(
            stream,
            f.runtime.instance_id(),
            Duration::from_secs(5),
            ClientLimits::default(),
        )
        .await
        .unwrap();
        assert!(timing.camera_timing(f.outputs[0]).await.unwrap().native);
        assert!(!timing.camera_timing(f.outputs[2]).await.unwrap().native);
        assert!(
            f.runtime
                .source_snapshots()
                .iter()
                .all(|source| source.lease_count == 0)
        );
        let client = f.runtime.client();
        client.connect(f.outputs[2]).await.unwrap();
        let outer = client.connection(f.outputs[2]).unwrap();
        let camera = outer.camera().unwrap();
        // The direct device reports a 64-by-64 minimum ROI. Use a region
        // admitted by both native transports rather than relaxing their rules.
        camera.set(S::NumX(64)).await.unwrap();
        camera.set(S::NumY(64)).await.unwrap();
        let image = capture(camera, 0.02).await;
        assert!(image.image.native().is_some());
        let baseline = f.resources.image_budget().used_bytes();
        client.connect(f.outputs[0]).await.unwrap();
        let inner = client.connection(f.outputs[0]).unwrap();
        let original = inner.camera().unwrap().image().unwrap();
        assert_eq!(
            original.image.bytes().as_ptr(),
            image.image.bytes().as_ptr()
        );
        assert_eq!(
            original.identity.exposure.start_time,
            image.identity.exposure.start_time
        );
        assert_eq!(baseline, f.resources.image_budget().used_bytes());
        assert!(baseline >= image.image.bytes().len());
        drop(original);
        drop(image);
        client.close();
        drop(inner);
        drop(outer);
        timing.close();
        serving.await.unwrap().unwrap();
        f.finish().await;
        assert_eq!(f.resources.image_budget().used_bytes(), 0);
    }
}

#[tokio::test(start_paused = true)]
async fn nested_camera_download_failure_publishes_no_pixels_and_retains_uncertainty() {
    for capacity in [false, true] {
        let f = Fixture::new(if capacity { FRAME - 1 } else { FRAME * 4 });
        if !capacity {
            f.runtime
                .update_simulation(
                    f.sources[0],
                    serde_json::from_value(json!({"fault":"imageError"})).unwrap(),
                )
                .await
                .unwrap();
        }
        let client = f.runtime.client();
        client.connect(f.outputs[2]).await.unwrap();
        let outer = client.connection(f.outputs[2]).unwrap();
        let camera = outer.camera().unwrap();
        camera
            .start(ExposureRequest {
                duration_seconds: 0.1,
                light: true,
            })
            .await
            .unwrap();
        until(|| camera.status().phase == AcquisitionPhase::Uncertain).await;
        assert!(!camera.status().image_ready);
        assert!(camera.image().is_err());
        assert_eq!(camera.abort().await.unwrap_err().kind, ErrorKind::Uncertain);
        assert_eq!(
            camera
                .start(ExposureRequest {
                    duration_seconds: 0.1,
                    light: true
                })
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.resources.image_budget().used_bytes(), 0);
        client.close();
        drop(outer);
        f.finish().await;
    }
}

#[tokio::test(start_paused = true)]
async fn virtual_camera_missing_exposure_metadata_keeps_pixels_and_original_optional_errors() {
    let f = Fixture::new(FRAME * 4);
    f.runtime
        .update_simulation(
            f.sources[0],
            serde_json::from_value(json!({"camera":{"exposureMetadataAvailable":false}})).unwrap(),
        )
        .await
        .unwrap();
    let client = f.runtime.client();
    client.connect(f.outputs[2]).await.unwrap();
    let outer = client.connection(f.outputs[2]).unwrap();
    let camera = outer.camera().unwrap();
    let image = capture(camera, 0.1).await;
    assert!(image.identity.exposure.duration_seconds.is_none());
    assert!(image.identity.exposure.start_time.is_none());
    for property in [P::LastExposureDuration, P::LastExposureStartTime] {
        assert_eq!(
            camera.property(property).await.unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }
    assert_eq!(image.image.bytes().len(), FRAME);
    drop(image);
    client.close();
    drop(outer);
    f.finish().await;
    assert_eq!(f.resources.image_budget().used_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn uncertain_inner_setting_retires_every_virtual_generation_without_replay() {
    let f = Fixture::new(FRAME * 4);
    let client = f.runtime.client();
    client.connect(f.outputs[2]).await.unwrap();
    let outer = client.connection(f.outputs[2]).unwrap();
    let camera = outer.camera().unwrap();
    let image = capture(camera, 0.1).await;
    // Keep independent leases at every inner layer. Otherwise virtual reset
    // closes the last inner lease and ordinary disconnect retires that latch.
    let observer = f.runtime.client();
    for output in &f.outputs[..2] {
        observer.connect(*output).await.unwrap();
    }
    let generations: Vec<_> = f
        .sources
        .iter()
        .map(|id| f.runtime.source_snapshot(*id).unwrap().generation)
        .collect();
    f.runtime
        .update_simulation(
            f.sources[0],
            serde_json::from_value(json!({"fault":"uncertainWrite"})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        camera.set(S::Gain(12)).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    for (id, generation) in f.sources.iter().zip(generations) {
        let source = f.runtime.source_snapshot(*id).unwrap();
        assert_ne!(source.generation, generation);
        assert!(
            source.write_uncertain,
            "Lost uncertainty with source state: {source:?}"
        );
    }
    assert!(!outer.connected());
    assert!(camera.image().is_err());
    assert_eq!(
        camera.set(S::Gain(13)).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        f.runtime
            .source_snapshot(f.sources[0])
            .unwrap()
            .simulation
            .unwrap()
            .camera
            .unwrap()
            .gain,
        12
    );
    for output in &f.outputs[..2] {
        assert!(!observer.connection(*output).unwrap().connected());
    }
    assert_eq!(image.image.bytes().len(), FRAME);
    client.close();
    observer.close();
    drop(outer);
    f.finish().await;
    assert_eq!(f.resources.image_budget().used_bytes(), FRAME);
    drop(image);
    assert_eq!(f.resources.image_budget().used_bytes(), 0);
}
