//! Explicit simulator through the same source, controller and budget as cameras.
use regain_hub::{
    camera::{
        acquisition::{AcquisitionPhase as Phase, ExposureRequest, GuideRequest},
        image::{ElementType, ImageBudget, ImageOrder},
        properties::{CameraProperty as P, CameraSetting as S, CameraValue as V},
        runtime::CameraResources,
    },
    config::{DeviceType, HubConfig},
    factory::NoCredentials,
    native::NativeRuntime,
    runtime::HubRuntime,
    safety::MonotonicClock,
    simulated::{CameraUpdate, Fault, SimulatedBackend, SimulationUpdate},
    source::{Backend, ErrorKind, Values},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use uuid::Uuid;

fn patch(value: Value) -> SimulationUpdate {
    serde_json::from_value(value).unwrap()
}
async fn read(backend: &mut SimulatedBackend, member: &str) -> Value {
    backend.read(member.into(), Values::new()).await.unwrap()
}
async fn write(backend: &mut SimulatedBackend, member: &str, args: Value) {
    backend
        .write(member.into(), serde_json::from_value(args).unwrap())
        .await
        .unwrap();
}
fn simulator() -> SimulatedBackend {
    SimulatedBackend::new(
        DeviceType::Camera,
        P::ALL
            .iter()
            .map(|property| property.sample_request())
            .collect(),
    )
    .unwrap()
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

fn runtime(
    maximum: usize,
    sources: usize,
) -> (Arc<HubRuntime>, CameraResources, Vec<Uuid>, Vec<Uuid>) {
    let mut config = HubConfig::empty();
    let mut ids = Vec::new();
    let mut outputs = Vec::new();
    for index in 0..sources {
        let source = Uuid::new_v4();
        ids.push(source);
        config.sources.push(
            serde_json::from_value(json!({"id":source,"label":"Explicit camera simulation",
            "backend":{"kind":"simulated","deviceType":"camera"},
            "polling":{"pollSeconds":60.0,"requestTimeoutSeconds":1.0}}))
            .unwrap(),
        );
        for sibling in 0..2 {
            let id = Uuid::new_v4();
            outputs.push(id);
            config.outputs.push(
                serde_json::from_value(json!({"id":id,"label":"Simulation camera output",
                "number":index * 2 + sibling,
                "device":{"kind":"proxy","source":source,"deviceType":"camera"}}))
                .unwrap(),
            );
        }
    }
    let resources = CameraResources::new(maximum).unwrap();
    let native = NativeRuntime {
        directory: std::env::temp_dir().join("absent-simulated-camera-workers"),
        simulate: false,
        references: None,
        cameras: None,
    };
    let runtime = HubRuntime::build_with_camera_resources(
        config,
        &native,
        &NoCredentials,
        Arc::new(MonotonicClock::default()),
        resources.clone(),
    )
    .unwrap();
    (runtime, resources, ids, outputs)
}

#[test]
fn camera_controls_are_atomic_strict_and_class_specific() {
    let mut backend = simulator();
    assert!(backend.simulated());
    assert!(backend.native_camera_timing().is_none());
    assert!(backend.native_camera_resources().is_none());
    let before = serde_json::to_value(backend.simulation_status()).unwrap();
    for update in [
        CameraUpdate {
            readout_duration_seconds: Some(-1.0),
            ..Default::default()
        },
        CameraUpdate {
            readout_duration_seconds: Some(301.0),
            ..Default::default()
        },
        CameraUpdate {
            readout_duration_seconds: Some(f64::NAN),
            ..Default::default()
        },
        CameraUpdate {
            temperature: Some(-273.16),
            ..Default::default()
        },
        CameraUpdate {
            heat_sink_temperature: Some(f64::INFINITY),
            ..Default::default()
        },
    ] {
        assert_eq!(
            backend
                .update_simulation(SimulationUpdate {
                    camera: Some(update),
                    fault: Some(Fault::ImageError),
                    ..Default::default()
                })
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            serde_json::to_value(backend.simulation_status()).unwrap(),
            before
        );
    }
    for update in [
        json!({"camera":{"readoutDurationSeconds":"0"}}),
        json!({"camera":{"hasShutter":1}}),
        json!({"camera":{"imageReady":true}}),
        json!({"camera":{"gain":5}}),
    ] {
        assert!(serde_json::from_value::<SimulationUpdate>(update).is_err());
    }
    let mut focuser = SimulatedBackend::new(DeviceType::Focuser, vec![]).unwrap();
    for update in [
        json!({"camera":{}}),
        json!({"fault":"stalledExposure"}),
        json!({"fault":"imageError"}),
        json!({"fault":"invalidImage"}),
    ] {
        assert_eq!(
            focuser.update_simulation(patch(update)).unwrap_err().kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(
        backend
            .update_simulation(patch(json!({"fault":"stalledMotion"})))
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
}

#[tokio::test(start_paused = true)]
async fn simulated_guiding_uses_monotonic_time_and_is_independent_of_capture_and_connection() {
    let mut backend = simulator();
    backend.connect().await.unwrap();
    assert_eq!(read(&mut backend, "canpulseguide").await, json!(false));
    assert_eq!(
        backend
            .read("ispulseguiding".into(), Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        backend
            .write(
                "pulseguide".into(),
                serde_json::from_value(json!({"Direction":0,"Duration":100})).unwrap()
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    backend
        .update_simulation(patch(
            json!({"camera":{"canPulseGuide":true,"readoutDurationSeconds":0.0}}),
        ))
        .unwrap();
    write(
        &mut backend,
        "pulseguide",
        json!({"Direction":0,"Duration":100}),
    )
    .await;
    assert_eq!(read(&mut backend, "ispulseguiding").await, json!(true));
    write(
        &mut backend,
        "startexposure",
        json!({"Duration":1.0,"Light":true}),
    )
    .await;
    write(&mut backend, "abortexposure", json!({})).await;
    assert_eq!(read(&mut backend, "ispulseguiding").await, json!(true));
    tokio::time::advance(Duration::from_millis(99)).await;
    assert_eq!(read(&mut backend, "ispulseguiding").await, json!(true));
    backend.disconnect().await.unwrap();
    tokio::time::advance(Duration::from_millis(1)).await;
    backend.connect().await.unwrap();
    assert_eq!(read(&mut backend, "ispulseguiding").await, json!(false));
    for direction in 0..4 {
        write(
            &mut backend,
            "pulseguide",
            json!({"Direction":direction,"Duration":0}),
        )
        .await;
        assert_eq!(read(&mut backend, "ispulseguiding").await, json!(false));
    }
}

#[tokio::test(start_paused = true)]
async fn simulated_guide_retains_runtime_activity_after_client_loss_and_shutdown_joins_monitor() {
    let (runtime, resources, ids, outputs) = runtime(1_000_000, 1);
    runtime
        .update_simulation(ids[0], patch(json!({"camera":{"canPulseGuide":true}})))
        .await
        .unwrap();
    let first = runtime.client();
    first.connect(outputs[0]).await.unwrap();
    let connection = first.connection(outputs[0]).unwrap();
    let camera = connection.camera().unwrap();
    camera
        .pulse_guide(GuideRequest {
            direction: 3,
            duration_milliseconds: i32::MAX,
        })
        .await
        .unwrap();
    first.close();
    drop(connection);
    assert!(resources.activity().active() > 0);
    assert_eq!(
        runtime
            .update_simulation(ids[0], patch(json!({"camera":{"canPulseGuide":false}})))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    runtime.shutdown().await.unwrap();
    // This is immediate after the awaited drain; no yielding cleanup loop.
    assert_eq!(resources.activity().active(), 0);
    assert!(!runtime.source_snapshot(ids[0]).unwrap().transport_connected);
}

#[tokio::test(start_paused = true)]
async fn camera_exposure_and_readout_use_monotonic_time_and_frozen_subframes() {
    let mut backend = simulator();
    backend.connect().await.unwrap();
    backend
        .update_simulation(patch(json!({"camera":{"readoutDurationSeconds":2.0}})))
        .unwrap();
    for (member, value) in [
        ("numx", json!({"NumX":3})),
        ("numy", json!({"NumY":2})),
        ("binx", json!({"BinX":2})),
        ("biny", json!({"BinY":3})),
        ("startx", json!({"StartX":1})),
        ("starty", json!({"StartY":2})),
    ] {
        write(&mut backend, member, value).await;
    }
    let before_start = chrono::Utc::now();
    write(
        &mut backend,
        "startexposure",
        json!({"Duration":10.0,"Light":true}),
    )
    .await;
    let after_start = chrono::Utc::now();
    assert_eq!(read(&mut backend, "imageready").await, false);
    assert_eq!(read(&mut backend, "camerastate").await, 2);
    assert_eq!(
        backend
            .write(
                "gain".into(),
                serde_json::from_value(json!({"Gain":1})).unwrap()
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    assert_eq!(
        backend
            .camera_image(ImageBudget::new(100).unwrap())
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Unavailable
    );
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(read(&mut backend, "percentcompleted").await, 50);
    write(&mut backend, "cooleron", json!({"CoolerOn":true})).await;
    write(
        &mut backend,
        "setccdtemperature",
        json!({"SetCCDTemperature":-20.0}),
    )
    .await;
    assert_eq!(read(&mut backend, "ccdtemperature").await, 12.0);
    // The changed duration applies to the next exposure, without replacing this one.
    backend
        .update_simulation(patch(json!({"camera":{"readoutDurationSeconds":0.0}})))
        .unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    assert_eq!(read(&mut backend, "camerastate").await, 3);
    assert_eq!(read(&mut backend, "imageready").await, false);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert_eq!(read(&mut backend, "camerastate").await, 0);
    assert_eq!(read(&mut backend, "imageready").await, true);
    assert_eq!(read(&mut backend, "lastexposureduration").await, 10.0);
    let timestamp = read(&mut backend, "lastexposurestarttime").await;
    P::LastExposureStartTime.decode(&timestamp).unwrap();
    let captured =
        chrono::NaiveDateTime::parse_from_str(timestamp.as_str().unwrap(), "%Y-%m-%dT%H:%M:%S%.f")
            .unwrap()
            .and_utc();
    assert!(captured >= before_start && captured <= after_start);
    assert!(!timestamp.as_str().unwrap().ends_with('Z')); // FITS times are implicitly UTC.
    let budget = ImageBudget::new(24).unwrap();
    let image = backend.camera_image(budget.clone()).await.unwrap();
    assert_eq!(image.descriptor().width(), 3);
    assert_eq!(image.descriptor().height(), 2);
    assert_eq!(image.descriptor().element_type(), ElementType::Int32);
    assert_eq!(image.descriptor().transmission_type(), ElementType::UInt16);
    assert_eq!(image.descriptor().order(), ImageOrder::SensorRows);
    assert_eq!(
        image.bytes(),
        [331u16, 365, 399, 424, 458, 492]
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>()
    );
    assert_eq!(budget.used_bytes(), 12);
    let repeat = backend.camera_image(budget.clone()).await.unwrap();
    assert_eq!(repeat.bytes(), image.bytes());
    assert_eq!(read(&mut backend, "lastexposurestarttime").await, timestamp);
    drop(image);
    drop(repeat);
    assert_eq!(budget.used_bytes(), 0);
    write(
        &mut backend,
        "startexposure",
        json!({"Duration":0.0,"Light":false}),
    )
    .await;
    P::LastExposureStartTime
        .decode(&read(&mut backend, "lastexposurestarttime").await)
        .unwrap();
    let dark = backend.camera_image(budget).await.unwrap();
    assert!(
        dark.bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .all(|bytes| *bytes == 12u16.to_le_bytes())
    );
}

#[tokio::test(start_paused = true)]
async fn camera_modes_preserve_rgb_planes_and_rank_three_one_plane() {
    let mut backend = simulator();
    backend.connect().await.unwrap();
    backend
        .update_simulation(patch(json!({"camera":{"readoutDurationSeconds":0.0}})))
        .unwrap();
    write(&mut backend, "numx", json!({"NumX":3})).await;
    write(&mut backend, "numy", json!({"NumY":2})).await;
    for (mode, planes) in [(0, None), (1, Some(3)), (2, Some(1))] {
        write(&mut backend, "readoutmode", json!({"ReadoutMode":mode})).await;
        write(
            &mut backend,
            "startexposure",
            json!({"Duration":0.0,"Light":false}),
        )
        .await;
        let budget = ImageBudget::new(100).unwrap();
        let image = backend.camera_image(budget.clone()).await.unwrap();
        assert_eq!(image.descriptor().planes(), planes);
        assert_eq!(image.bytes().len(), 12 * planes.unwrap_or(1) as usize);
        assert_eq!(
            read(&mut backend, "sensortype").await,
            if mode == 1 { 1 } else { 0 }
        );
        for member in ["bayeroffsetx", "bayeroffsety"] {
            let value = backend.read(member.into(), Values::new()).await;
            if mode == 1 {
                assert_eq!(value.unwrap(), 0);
            } else {
                assert_eq!(value.unwrap_err().kind, ErrorKind::Unsupported);
            }
        }
        for property in P::ALL {
            match backend.read(property.member().into(), Values::new()).await {
                Ok(value) => {
                    property.decode(&value).unwrap();
                }
                Err(error) => {
                    assert_eq!(error.kind, ErrorKind::Unsupported);
                    assert!(matches!(
                        property,
                        P::Gains
                            | P::Offsets
                            | P::SubExposureDuration
                            | P::IsPulseGuiding
                            | P::BayerOffsetX
                            | P::BayerOffsetY
                    ));
                    if matches!(property, P::BayerOffsetX | P::BayerOffsetY) {
                        assert_ne!(mode, 1);
                    }
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn camera_stop_preserves_partial_duration_abort_discards_and_disconnect_does_not_abort() {
    let mut backend = simulator();
    backend.connect().await.unwrap();
    backend
        .update_simulation(patch(json!({"camera":{"readoutDurationSeconds":1.0}})))
        .unwrap();
    write(
        &mut backend,
        "startexposure",
        json!({"Duration":10.0,"Light":true}),
    )
    .await;
    tokio::time::advance(Duration::from_secs(2)).await;
    write(&mut backend, "stopexposure", json!({})).await;
    assert_eq!(read(&mut backend, "camerastate").await, 3);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(read(&mut backend, "lastexposureduration").await, 2.0);
    write(
        &mut backend,
        "startexposure",
        json!({"Duration":10.0,"Light":true}),
    )
    .await;
    write(&mut backend, "abortexposure", json!({})).await;
    tokio::time::advance(Duration::from_secs(11)).await;
    assert_eq!(read(&mut backend, "imageready").await, false);
    assert_eq!(read(&mut backend, "camerastate").await, 0);
    write(
        &mut backend,
        "startexposure",
        json!({"Duration":10.0,"Light":true}),
    )
    .await;
    backend.disconnect().await.unwrap();
    tokio::time::advance(Duration::from_secs(11)).await;
    backend.connect().await.unwrap();
    assert_eq!(read(&mut backend, "imageready").await, true);
    assert_eq!(read(&mut backend, "lastexposureduration").await, 10.0);
}

#[tokio::test]
async fn camera_setters_reject_malformed_or_out_of_range_values_without_mutation() {
    let mut backend = simulator();
    backend.connect().await.unwrap();
    for (member, args) in [
        ("binx", json!({"BinX":5})),
        ("biny", json!({"BinY":0})),
        ("numx", json!({"NumX":0})),
        ("startx", json!({"StartX":-1})),
        ("gain", json!({"Gain":601})),
        ("offset", json!({"Offset":-1})),
        ("readoutmode", json!({"ReadoutMode":3})),
        ("fastreadout", json!({"FastReadout":1})),
        ("cooleron", json!({"CoolerOn":"true"})),
        ("setccdtemperature", json!({"SetCCDTemperature":61.0})),
        ("gain", json!({"Gain":1,"extra":true})),
        ("startexposure", json!({"Duration":0.0,"Light":true})),
        ("startexposure", json!({"Duration":3601.0,"Light":true})),
        ("startexposure", json!({"Duration":1.0,"Light":0})),
        ("abortexposure", json!({"extra":true})),
    ] {
        let before = backend.simulation_status().unwrap().camera;
        assert_eq!(
            backend
                .write(member.into(), serde_json::from_value(args).unwrap())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(backend.simulation_status().unwrap().camera, before);
    }
    // Individual valid setters may temporarily form an invalid combination.
    write(&mut backend, "binx", json!({"BinX":2})).await;
    assert_eq!(
        backend
            .write(
                "startexposure".into(),
                serde_json::from_value(json!({"Duration":1.0,"Light":true})).unwrap()
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    assert!(
        !backend
            .simulation_status()
            .unwrap()
            .camera
            .unwrap()
            .image_ready
    );
}

#[tokio::test(start_paused = true)]
async fn desired_roi_bounds_are_rejected_at_capture_without_allocation_or_image_replacement() {
    let (runtime, resources, _, outputs) = runtime(1_000_000, 1);
    let client = runtime.client();
    client.connect(outputs[0]).await.unwrap();
    let connection = client.connection(outputs[0]).unwrap();
    let camera = connection.camera().unwrap();
    camera.set(S::NumX(3)).await.unwrap();
    camera.set(S::NumY(2)).await.unwrap();
    let request = ExposureRequest {
        duration_seconds: 0.0,
        light: false,
    };
    camera.start(request).await.unwrap();
    until(|| camera.status().image_ready).await;
    let frame = camera.image().unwrap();
    let held = resources.image_budget().used_bytes();
    for setting in [
        S::NumX(321),
        S::NumY(241),
        S::StartX(320),
        S::StartY(240),
        S::NumX(i32::MAX),
        S::StartY(i32::MAX),
    ] {
        camera.set(S::NumX(3)).await.unwrap();
        camera.set(S::NumY(2)).await.unwrap();
        camera.set(S::StartX(0)).await.unwrap();
        camera.set(S::StartY(0)).await.unwrap();
        camera.set(setting).await.unwrap();
        assert_eq!(
            camera.start(request).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(camera.status().phase, Phase::Idle);
        assert!(camera.status().image_ready);
        assert!(Arc::ptr_eq(&frame, &camera.image().unwrap()));
        assert_eq!(resources.image_budget().used_bytes(), held);
    }
    client.close();
    drop(connection);
    runtime.shutdown().await.unwrap();
    drop(frame);
    assert_eq!(resources.image_budget().used_bytes(), 0);
}

#[tokio::test]
async fn camera_optional_properties_and_controls_report_unsupported_without_thermal_claims() {
    let mut backend = simulator();
    backend.connect().await.unwrap();
    backend
        .update_simulation(patch(
            json!({"camera":{"canAbortExposure":false,"canStopExposure":false,
        "canFastReadout":false,"canSetCcdTemperature":false,"canGetCoolerPower":false,
        "temperatureAvailable":false,"exposureMetadataAvailable":false}}),
        ))
        .unwrap();
    for member in [
        "fastreadout",
        "cooleron",
        "coolerpower",
        "setccdtemperature",
        "ccdtemperature",
        "heatsinktemperature",
        "lastexposureduration",
        "lastexposurestarttime",
        "subexposureduration",
    ] {
        assert_eq!(
            backend
                .read(member.into(), Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
    }
    for (member, args) in [
        ("abortexposure", json!({})),
        ("stopexposure", json!({})),
        ("fastreadout", json!({"FastReadout":true})),
        ("cooleron", json!({"CoolerOn":true})),
        ("setccdtemperature", json!({"SetCCDTemperature":-20.0})),
    ] {
        assert_eq!(
            backend
                .write(member.into(), serde_json::from_value(args).unwrap())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
    }
    // Removing an unrelated feature cannot turn an invalid temperature into Unsupported.
    backend
        .update_simulation(patch(json!({"camera":{"canSetCcdTemperature":true}})))
        .unwrap();
    assert_eq!(
        backend
            .write(
                "setccdtemperature".into(),
                serde_json::from_value(json!({"SetCCDTemperature":61.0})).unwrap()
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
}

#[tokio::test(start_paused = true)]
async fn simulated_runtime_shares_acquisition_and_pins_images_across_captures_and_shutdown() {
    let (runtime, resources, ids, outputs) = runtime(1_000_000, 1);
    assert!(runtime.source_snapshot(ids[0]).unwrap().simulated);
    assert_eq!(runtime.source_snapshot(ids[0]).unwrap().lease_count, 0);
    let first = runtime.client();
    let observer = runtime.client();
    first.connect(outputs[0]).await.unwrap();
    observer.connect(outputs[1]).await.unwrap();
    let a = first.connection(outputs[0]).unwrap();
    let b = observer.connection(outputs[1]).unwrap();
    let camera = a.camera().unwrap();
    let other = b.camera().unwrap();
    for setting in [S::NumX(3), S::NumY(2), S::ReadoutMode(1)] {
        camera.set(setting).await.unwrap();
    }
    let acquisition = camera
        .start(ExposureRequest {
            duration_seconds: 5.0,
            light: true,
        })
        .await
        .unwrap();
    assert_eq!(
        other
            .start(ExposureRequest {
                duration_seconds: 1.0,
                light: true
            })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    assert_eq!(other.abort().await.unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(
        camera.set(S::Gain(3)).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    camera.set(S::CoolerOn(true)).await.unwrap();
    until(|| camera.status().image_ready).await;
    let image = camera.image().unwrap();
    let repeated = other.image().unwrap();
    assert!(Arc::ptr_eq(&image, &repeated));
    assert_eq!(image.identity.acquisition, acquisition);
    assert_eq!(resources.image_budget().used_bytes(), 36);
    assert_eq!(image.image.descriptor().planes(), Some(3));
    let pinned_bytes = image.image.bytes().to_vec();
    camera
        .start(ExposureRequest {
            duration_seconds: 0.0,
            light: false,
        })
        .await
        .unwrap();
    until(|| camera.status().image_ready).await;
    let next = camera.image().unwrap();
    assert_ne!(next.identity.acquisition, acquisition);
    assert_ne!(next.image.bytes(), pinned_bytes);
    assert_eq!(image.image.bytes(), pinned_bytes);
    assert_eq!(resources.image_budget().used_bytes(), 72);
    first.close();
    observer.close();
    drop(a);
    drop(b);
    runtime.shutdown().await.unwrap();
    assert_eq!(resources.image_budget().used_bytes(), 72);
    drop(image);
    drop(repeated);
    assert_eq!(resources.image_budget().used_bytes(), 36);
    drop(next);
    assert_eq!(resources.image_budget().used_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn simulated_runtime_image_failures_remain_uncertain_without_replay_or_partial_publication() {
    for (fault, maximum) in [
        (Fault::ImageError, 1_000_000),
        (Fault::InvalidImage, 1_000_000),
        (Fault::None, 1),
    ] {
        let (runtime, resources, ids, outputs) = runtime(maximum, 1);
        runtime
            .update_simulation(
                ids[0],
                SimulationUpdate {
                    fault: Some(fault),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let client = runtime.client();
        client.connect(outputs[0]).await.unwrap();
        let connection = client.connection(outputs[0]).unwrap();
        let camera = connection.camera().unwrap();
        let acquisition = camera
            .start(ExposureRequest {
                duration_seconds: 0.0,
                light: false,
            })
            .await
            .unwrap();
        until(|| camera.status().phase == Phase::Uncertain).await;
        assert_eq!(camera.status().acquisition, Some(acquisition));
        assert!(!camera.status().image_ready);
        assert!(camera.image().is_err());
        assert_eq!(resources.image_budget().used_bytes(), 0);
        assert_eq!(
            runtime
                .update_simulation(
                    ids[0],
                    SimulationUpdate {
                        fault: Some(Fault::None),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Busy
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(camera.status().phase, Phase::Uncertain);
        assert_eq!(
            camera
                .start(ExposureRequest {
                    duration_seconds: 0.0,
                    light: false
                })
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Uncertain
        );
        camera.abandon_uncertain().unwrap();
        runtime
            .update_simulation(ids[0], patch(json!({"fault":"none"})))
            .await
            .unwrap();
        assert!(!camera.status().image_ready);
        client.close();
        drop(connection);
        runtime.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn simulated_runtime_stop_and_abort_keep_owner_rules_and_optional_metadata() {
    let (runtime, _, ids, outputs) = runtime(1_000_000, 1);
    runtime
        .update_simulation(
            ids[0],
            patch(json!({"camera":{"exposureMetadataAvailable":false}})),
        )
        .await
        .unwrap();
    let client = runtime.client();
    client.connect(outputs[0]).await.unwrap();
    let connection = client.connection(outputs[0]).unwrap();
    let camera = connection.camera().unwrap();
    camera
        .start(ExposureRequest {
            duration_seconds: 20.0,
            light: true,
        })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    camera.stop().await.unwrap();
    until(|| camera.status().image_ready).await;
    let image = camera.image().unwrap();
    assert_eq!(image.identity.exposure.duration_seconds, None);
    assert_eq!(
        image
            .identity
            .exposure
            .duration_error
            .as_ref()
            .unwrap()
            .kind,
        ErrorKind::Unsupported
    );
    assert!(
        matches!(camera.property(P::LastExposureDuration).await,Err(error) if error.kind==ErrorKind::Unsupported)
    );
    runtime
        .update_simulation(ids[0], patch(json!({"fault":"stalledExposure"})))
        .await
        .unwrap();
    camera
        .start(ExposureRequest {
            duration_seconds: 20.0,
            light: true,
        })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(2)).await;
    camera.abort().await.unwrap();
    assert_eq!(camera.status().phase, Phase::Idle);
    assert!(!camera.status().image_ready);
    client.close();
    drop(connection);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn simulated_runtime_uncertain_start_is_not_replayed_by_fault_clear() {
    let (runtime, _, ids, outputs) = runtime(1_000_000, 1);
    let client = runtime.client();
    client.connect(outputs[0]).await.unwrap();
    let connection = client.connection(outputs[0]).unwrap();
    let camera = connection.camera().unwrap();
    runtime
        .update_simulation(ids[0], patch(json!({"fault":"uncertainWrite"})))
        .await
        .unwrap();
    assert_eq!(
        camera
            .start(ExposureRequest {
                duration_seconds: 10.0,
                light: true
            })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        runtime
            .update_simulation(ids[0], patch(json!({"fault":"none"})))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    tokio::time::advance(Duration::from_secs(11)).await;
    assert_eq!(camera.status().phase, Phase::Uncertain);
    assert!(!camera.status().image_ready);
    assert_eq!(
        camera
            .start(ExposureRequest {
                duration_seconds: 0.0,
                light: false
            })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Disconnected
    );
    // Explicit local abandonment sends no Abort and cannot publish the unknown frame.
    camera.abandon_uncertain().unwrap();
    runtime
        .update_simulation(ids[0], patch(json!({"fault":"none"})))
        .await
        .unwrap();
    assert_eq!(
        camera
            .start(ExposureRequest {
                duration_seconds: 0.0,
                light: false
            })
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Disconnected
    );
    assert!(runtime.source_snapshot(ids[0]).unwrap().write_uncertain);
    assert!(camera.image().is_err());
    client.close();
    drop(connection);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn simulated_runtime_independent_cameras_acquire_concurrently_without_sdk_or_workers() {
    let (runtime, _, ids, outputs) = runtime(1_000_000, 2);
    let client = runtime.client();
    client.connect(outputs[0]).await.unwrap();
    client.connect(outputs[2]).await.unwrap();
    let first = client.connection(outputs[0]).unwrap();
    let second = client.connection(outputs[2]).unwrap();
    let a = first.camera().unwrap();
    let b = second.camera().unwrap();
    a.start(ExposureRequest {
        duration_seconds: 10.0,
        light: true,
    })
    .await
    .unwrap();
    b.start(ExposureRequest {
        duration_seconds: 5.0,
        light: true,
    })
    .await
    .unwrap();
    until(|| a.status().image_ready && b.status().image_ready).await;
    assert_eq!(a.image().unwrap().identity.source, ids[0]);
    assert_eq!(b.image().unwrap().identity.source, ids[1]);
    assert_eq!(
        a.property(P::CoolerOn).await.unwrap(),
        V::Boolean { value: false }
    );
    client.close();
    drop(first);
    drop(second);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn simulation_update_ack_releases_control_on_success_and_rejection() {
    let (runtime, _, ids, outputs) = runtime(1_000_000, 1);
    let client = runtime.client();
    client.connect(outputs[0]).await.unwrap();
    let connection = client.connection(outputs[0]).unwrap();
    let camera = connection.camera().unwrap();
    runtime
        .update_simulation(ids[0], patch(json!({"camera":{"temperature":-10.0}})))
        .await
        .unwrap();
    camera.set(S::Gain(200)).await.unwrap();
    assert_eq!(
        runtime
            .update_simulation(ids[0], patch(json!({"camera":{"temperature":-300.0}})))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    camera.set(S::Gain(300)).await.unwrap();
    assert_eq!(
        camera.property(P::Gain).await.unwrap(),
        V::Integer { value: 300 }
    );
    assert_eq!(
        camera.property(P::CcdTemperature).await.unwrap(),
        V::Number { value: -10.0 }
    );
    client.close();
    drop(connection);
    runtime.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn simulated_stalled_readiness_requires_explicit_abandonment_before_test_control_changes() {
    let (runtime, resources, ids, outputs) = runtime(1_000_000, 1);
    runtime
        .update_simulation(ids[0], patch(json!({"fault":"stalledExposure"})))
        .await
        .unwrap();
    let client = runtime.client();
    client.connect(outputs[0]).await.unwrap();
    let connection = client.connection(outputs[0]).unwrap();
    let camera = connection.camera().unwrap();
    camera
        .start(ExposureRequest {
            duration_seconds: 0.0,
            light: false,
        })
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(31)).await;
    until(|| camera.status().phase == Phase::Uncertain).await;
    assert_eq!(resources.image_budget().used_bytes(), 0);
    assert!(!camera.status().image_ready);
    assert_eq!(camera.abort().await.unwrap_err().kind, ErrorKind::Uncertain);
    assert_eq!(
        runtime
            .update_simulation(ids[0], patch(json!({"fault":"none"})))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Busy
    );
    camera.abandon_uncertain().unwrap();
    runtime
        .update_simulation(ids[0], patch(json!({"fault":"none"})))
        .await
        .unwrap();
    assert!(camera.image().is_err());
    camera
        .start(ExposureRequest {
            duration_seconds: 0.0,
            light: false,
        })
        .await
        .unwrap();
    until(|| camera.status().image_ready).await;
    client.close();
    drop(connection);
    runtime.shutdown().await.unwrap();
}
