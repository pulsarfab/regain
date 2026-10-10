use super::*;
use regain_hub::camera::{
    acquisition::{ExposureRequest, GuideRequest, GuidingPhase},
    image::{ElementType, ImageBudget, ImageOrder},
    properties::{CameraProperty as P, CameraSetting as S, CameraValue as V},
    runtime::CameraResources,
};

#[tokio::test]
async fn registered_camera_guide_shares_exposure_control_and_survives_owner_disconnect() {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        f.clear("Camera", json!({"version":4}));
        let config = camera_config(f.source("Camera", DeviceType::Camera, bitness));
        let hub = HubRuntime::build(
            config.clone(),
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let first = hub.client();
        let second = hub.client();
        first.connect(config.outputs[0].id).await.unwrap();
        second.connect(config.outputs[1].id).await.unwrap();
        let a = first.connection(config.outputs[0].id).unwrap();
        let b = second.connection(config.outputs[1].id).unwrap();
        let camera = a.camera().unwrap();
        let observer = b.camera().unwrap();
        camera
            .pulse_guide(GuideRequest {
                direction: 2,
                duration_milliseconds: i32::MAX,
            })
            .await
            .unwrap();
        camera.set(S::Gain(1)).await.unwrap_err();
        camera
            .start(ExposureRequest {
                duration_seconds: 0.01,
                light: true,
            })
            .await
            .unwrap();
        until(|| camera.status().image_ready).await;
        assert_eq!(
            camera.status().guiding.unwrap().phase,
            GuidingPhase::Guiding
        );
        assert_eq!(
            observer
                .pulse_guide(GuideRequest {
                    direction: 0,
                    duration_milliseconds: 0
                })
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Busy
        );
        first.close();
        drop(a);
        assert_eq!(f.count("Camera", "Disconnect"), 0);
        f.state("Camera", json!({"version":4,"cameraIsPulseGuiding":false}));
        until(|| observer.status().guiding.is_none()).await;
        observer.set(S::Gain(1)).await.unwrap();
        assert_eq!(f.count("Camera", "PulseGuide"), 1);
        assert_eq!(f.count("Camera", "Applied.PulseGuide"), 1);
        assert_eq!(f.count("Camera", "StartExposure"), 1);
        assert_eq!(f.count("Camera", "AbortExposure"), 0);
        assert_eq!(f.count("Camera", "StopExposure"), 0);
        second.close();
        drop(b);
        hub.shutdown().await.unwrap();
        assert_eq!(f.count("Camera", "Activate"), 1);
    }
}

#[tokio::test]
async fn registered_camera_lost_guide_reply_retains_uncertainty_without_replay_or_abort() {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        f.clear("Camera", json!({"version":4}));
        let config = camera_config(f.source("Camera", DeviceType::Camera, bitness));
        let hub = HubRuntime::build(
            config.clone(),
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let first = hub.client();
        let second = hub.client();
        first.connect(config.outputs[0].id).await.unwrap();
        second.connect(config.outputs[1].id).await.unwrap();
        let a = first.connection(config.outputs[0].id).unwrap();
        let b = second.connection(config.outputs[1].id).unwrap();
        f.state("Camera", json!({"version":4,"cameraLostReply":true}));
        assert_eq!(
            a.camera()
                .unwrap()
                .pulse_guide(GuideRequest {
                    direction: 1,
                    duration_milliseconds: 100
                })
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Uncertain
        );
        assert_eq!(
            b.camera().unwrap().status().guiding.unwrap().phase,
            GuidingPhase::Uncertain
        );
        assert!(
            hub.source_snapshot(config.sources[0].id)
                .unwrap()
                .write_uncertain
        );
        assert_eq!(
            b.camera().unwrap().set(S::Gain(1)).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.count("Camera", "PulseGuide"), 1);
        assert_eq!(f.count("Camera", "Applied.PulseGuide"), 1);
        assert_eq!(f.count("Camera", "AbortExposure"), 0);
        assert_eq!(f.count("Camera", "StopExposure"), 0);
        first.close();
        second.close();
        drop(a);
        drop(b);
        hub.shutdown().await.unwrap();
        assert_eq!(f.count("Camera", "PulseGuide"), 1);
    }
}

#[tokio::test]
async fn registered_camera_partial_body_cancellation_bad_header_and_trailer_release_budget_and_worker()
 {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        for fault in ["cancel", "header", "trailer"] {
            f.clear("Camera", json!({"version":4}));
            let source = f.source("Camera", DeviceType::Camera, bitness);
            let mut backend = f.backend(&source, Vec::new());
            let mut requests = 0;
            loop {
                requests += 1;
                if backend.connect_step().await.unwrap() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            backend
                .write(
                    "startexposure".into(),
                    Values::from([
                        ("Duration".into(), json!(0.01)),
                        ("Light".into(), json!(true)),
                    ]),
                )
                .await
                .unwrap();
            let pid = f.pid("Camera");
            let id = requests + 2;
            let descriptor = regain_hub::camera::image::ImageDescriptor::new(
                2,
                3,
                None,
                ElementType::Int32,
                ElementType::Int32,
                ImageOrder::Ascom,
            )
            .unwrap();
            let frame = json!({"ok":true,"result":{"protocol":1,"id":id,"value":descriptor,"error":null,
                "connection":{"deviceType":"camera","interfaceVersion":4,"method":"async","ownsConnection":true,"uncertain":false,"ready":true}}});
            let mut binary = descriptor
                .imagebytes_header(if fault == "header" { id + 1 } else { id }, 0)
                .to_vec();
            binary.extend(vec![0; if fault == "cancel" { 2 } else { 24 }]);
            if fault != "cancel" {
                binary.extend_from_slice(b"BADIMAGE");
            }
            let hex: String = binary.iter().map(|byte| format!("{byte:02x}")).collect();
            f.state(
                "Camera",
                json!({"version":4,"rawReplyMember":"ImageArray","rawFrame":frame.to_string(),
                "rawBytesHex":hex,"hangMember":"ImageArray"}),
            );
            let budget = ImageBudget::new(24).unwrap();
            if fault == "cancel" {
                {
                    let operation = backend.camera_image(budget.clone());
                    tokio::pin!(operation);
                    tokio::select! {
                        _ = &mut operation => panic!("Partial image finished without cancellation"),
                        _ = until(||budget.used_bytes()==24) => {}
                    }
                }
                // The body guard retires the stream on future destruction.
                assert_eq!(
                    backend
                        .read("name".into(), Values::new())
                        .await
                        .unwrap_err()
                        .kind,
                    ErrorKind::Transient
                );
            } else {
                assert!(
                    backend
                        .camera_image(budget.clone())
                        .await
                        .err()
                        .unwrap()
                        .transport_lost
                );
            }
            assert_eq!(budget.used_bytes(), 0);
            until(|| !alive(pid)).await;
            assert_eq!(f.count("Camera", "ImageArray"), 1);
            assert_eq!(f.count("Camera", "StartExposure"), 1);
            assert_eq!(f.count("Camera", "AbortExposure"), 0);
            assert_eq!(f.count("Camera", "StopExposure"), 0);
        }
    }
}

#[tokio::test]
async fn registered_camera_applied_lost_setting_reply_fences_all_clients_without_replay_or_abort() {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        f.clear("Camera", json!({"version":4}));
        let config = camera_config(f.source("Camera", DeviceType::Camera, bitness));
        let hub = HubRuntime::build(
            config.clone(),
            &f.native,
            &NoCredentials,
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let first = hub.client();
        let second = hub.client();
        first.connect(config.outputs[0].id).await.unwrap();
        second.connect(config.outputs[1].id).await.unwrap();
        let a = first.connection(config.outputs[0].id).unwrap();
        let b = second.connection(config.outputs[1].id).unwrap();
        f.state("Camera", json!({"version":4,"cameraLostReply":true}));
        assert_eq!(
            a.camera().unwrap().set(S::Gain(1)).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert!(
            hub.source_snapshot(config.sources[0].id)
                .unwrap()
                .write_uncertain
        );
        assert_eq!(
            b.camera().unwrap().set(S::Gain(0)).await.unwrap_err().kind,
            ErrorKind::Uncertain
        );
        assert_eq!(f.count("Camera", "Gain.set"), 1);
        assert_eq!(f.count("Camera", "Applied.Gain"), 1);
        assert_eq!(f.count("Camera", "StartExposure"), 0);
        assert_eq!(f.count("Camera", "AbortExposure"), 0);
        assert_eq!(f.count("Camera", "StopExposure"), 0);
        first.close();
        second.close();
        drop(a);
        drop(b);
        hub.shutdown().await.unwrap();
        assert_eq!(f.count("Camera", "Gain.set"), 1);
    }
}

fn camera_config(source: SourceConfig) -> HubConfig {
    let mut config = HubConfig::empty();
    for number in [2, 17] {
        config.outputs.push(OutputConfig {
            id: Uuid::new_v4(),
            number,
            label: format!("Private COM camera {number}"),
            device: VirtualDevice::Proxy {
                source: source.id,
                device_type: DeviceType::Camera,
            },
        });
    }
    config.sources.push(source);
    config
}

#[tokio::test]
async fn registered_camera_binary_all_types_ranks_and_multichunk_body_preserve_scalar_stream() {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        f.clear("Camera", json!({"version":4}));
        let source = f.source("Camera", DeviceType::Camera, bitness);
        let mut backend = f.backend(&source, Vec::new());
        backend.connect().await.unwrap();
        backend
            .write(
                "startexposure".into(),
                Values::from([
                    ("Duration".into(), json!(0.01)),
                    ("Light".into(), json!(true)),
                ]),
            )
            .await
            .unwrap();
        let budget = ImageBudget::new(8 * 128 * 129 * 3).unwrap();
        for element in [
            ElementType::Int16,
            ElementType::Int32,
            ElementType::Double,
            ElementType::Single,
            ElementType::UInt64,
            ElementType::Byte,
            ElementType::Int64,
            ElementType::UInt16,
            ElementType::UInt32,
        ] {
            for planes in [None, Some(1), Some(3)] {
                f.state(
                    "Camera",
                    json!({"version":4,"imageType":element,"imageWidth":128,"imageHeight":129,
                    "imagePlanes":planes.unwrap_or(0),"imageLowerBounds":true}),
                );
                let image = backend.camera_image(budget.clone()).await.unwrap();
                let descriptor = image.descriptor();
                assert_eq!(
                    (descriptor.width(), descriptor.height(), descriptor.planes()),
                    (128, 129, planes)
                );
                assert_eq!(descriptor.element_type(), element);
                assert_eq!(descriptor.transmission_type(), element);
                assert_eq!(descriptor.order(), ImageOrder::Ascom);
                assert_eq!(budget.used_bytes(), descriptor.byte_len());
                let expected: Vec<u8> = (1..=128 * 129 * planes.unwrap_or(1))
                    .flat_map(|index| match element {
                        ElementType::Int16 => (-(index as i32) % 30000).to_le_bytes()[..2].to_vec(),
                        ElementType::Int32 => (-(index as i32)).to_le_bytes().to_vec(),
                        ElementType::Double => (f64::from(index) + 0.25).to_le_bytes().to_vec(),
                        ElementType::Single => (index as f32 + 0.5).to_le_bytes().to_vec(),
                        ElementType::UInt64 => {
                            ((1u64 << 63) + u64::from(index)).to_le_bytes().to_vec()
                        }
                        ElementType::Byte => vec![index as u8],
                        ElementType::Int64 => (i64::MIN + i64::from(index)).to_le_bytes().to_vec(),
                        ElementType::UInt16 => {
                            (40000 + (index % 20000) as u16).to_le_bytes().to_vec()
                        }
                        ElementType::UInt32 => (0x8000_0000 + index).to_le_bytes().to_vec(),
                    })
                    .collect();
                assert_eq!(image.bytes(), expected);
                assert!(image.native().is_none());
                assert_eq!(
                    backend.read("name".into(), Values::new()).await.unwrap(),
                    "COM fixture"
                );
                drop(image);
                assert_eq!(budget.used_bytes(), 0);
            }
        }
        assert_eq!(f.count("Camera", "ImageArray"), 27);
        assert_eq!(f.count("Camera", "StartExposure"), 1);
        assert_eq!(f.count("Camera", "StopExposure"), 0);
        assert_eq!(f.count("Camera", "AbortExposure"), 0);
        backend.disconnect().await.unwrap();
        assert_eq!(f.count("Camera", "Disconnect"), 1);
    }
}

#[tokio::test]
async fn registered_camera_factory_shares_one_acquisition_image_settings_and_owned_connection() {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        for version in [2, 3, 4] {
            f.clear("Camera", json!({"version":version}));
            let config = camera_config(f.source("Camera", DeviceType::Camera, bitness));
            let resources = CameraResources::new(320 * 240 * 4 * 3).unwrap();
            let hub = HubRuntime::build_with_camera_resources(
                config.clone(),
                &f.native,
                &NoCredentials,
                Arc::new(MonotonicClock::default()),
                resources.clone(),
            )
            .unwrap();
            assert_eq!(f.count("Camera", "Activate"), 0);
            let owner = hub.client();
            let observer = hub.client();
            owner.connect(config.outputs[0].id).await.unwrap();
            observer.connect(config.outputs[1].id).await.unwrap();
            let a = owner.connection(config.outputs[0].id).unwrap();
            let b = observer.connection(config.outputs[1].id).unwrap();
            let camera = a.camera().unwrap();
            assert_eq!(
                camera.status().generation,
                b.camera().unwrap().status().generation
            );
            assert_eq!(f.count("Camera", "Activate"), 1);
            for setting in [
                S::BinX(2),
                S::NumX(4),
                S::NumY(3),
                S::Gain(1),
                S::Offset(1),
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
            let acquisition = camera
                .start(ExposureRequest {
                    duration_seconds: 0.01,
                    light: true,
                })
                .await
                .unwrap();
            until(|| {
                camera
                    .status()
                    .completed
                    .is_some_and(|image| image.acquisition == acquisition)
            })
            .await;
            let image = camera.image().unwrap();
            assert_eq!(image.image.bytes().len(), 4 * 3 * 4);
            assert_eq!(image.identity.exposure.duration_seconds, Some(0.01));
            assert_eq!(
                image.identity.exposure.start_time.as_deref(),
                Some("2026-10-07T01:02:03.1234567Z")
            );
            assert_eq!(
                image.image.bytes().as_ptr(),
                b.camera().unwrap().image().unwrap().image.bytes().as_ptr()
            );
            assert_eq!(f.count("Camera", "ImageArray"), 1);
            assert_eq!(resources.image_budget().used_bytes(), 48);
            owner.close();
            drop(a);
            assert!(b.connected());
            let next = b
                .camera()
                .unwrap()
                .start(ExposureRequest {
                    duration_seconds: 0.02,
                    light: false,
                })
                .await
                .unwrap();
            until(|| {
                b.camera()
                    .unwrap()
                    .status()
                    .completed
                    .is_some_and(|frame| frame.acquisition == next)
            })
            .await;
            let later = b.camera().unwrap().image().unwrap();
            assert_ne!(image.image.bytes(), later.image.bytes());
            assert_eq!(image.image.bytes()[..4], (-1i32).to_le_bytes());
            assert_eq!(f.count("Camera", "ImageArray"), 2);
            observer.close();
            drop(b);
            hub.shutdown().await.unwrap();
            assert_eq!(resources.image_budget().used_bytes(), 96);
            drop(image);
            drop(later);
            assert_eq!(resources.image_budget().used_bytes(), 0);
            assert_eq!(f.count("Camera", "StopExposure"), 0);
            assert_eq!(f.count("Camera", "AbortExposure"), 0);
            assert_eq!(f.count("Camera", "Dispose"), 0);
        }
    }
}

#[tokio::test]
async fn registered_camera_budget_rejection_and_hung_download_retire_without_replay_or_partial_image()
 {
    let Some(f) = Fixture::load() else { return };
    for bitness in [Bitness::X86, Bitness::X64] {
        for hung in [false, true] {
            f.clear("Camera", json!({"version":4}));
            let source = f.source("Camera", DeviceType::Camera, bitness);
            let mut backend = f.backend(&source, Vec::new());
            backend.connect().await.unwrap();
            backend
                .write(
                    "startexposure".into(),
                    Values::from([
                        ("Duration".into(), json!(0.01)),
                        ("Light".into(), json!(true)),
                    ]),
                )
                .await
                .unwrap();
            let pid = f.pid("Camera");
            let budget = ImageBudget::new(if hung { 320 * 240 * 4 } else { 1 }).unwrap();
            if hung {
                f.state("Camera", json!({"version":4,"hangMember":"ImageArray"}));
            }
            let error = backend.camera_image(budget.clone()).await.err().unwrap();
            assert!(error.transport_lost);
            assert_eq!(budget.used_bytes(), 0);
            until(|| !alive(pid)).await;
            assert_eq!(f.count("Camera", "StartExposure"), 1);
            assert_eq!(f.count("Camera", "ImageArray"), 1);
            assert_eq!(f.count("Camera", "AbortExposure"), 0);
            assert!(backend.connection_info().unwrap().uncertain);
            assert_eq!(
                backend
                    .read("name".into(), Values::new())
                    .await
                    .unwrap_err()
                    .kind,
                ErrorKind::Disconnected
            );
        }
    }
}
