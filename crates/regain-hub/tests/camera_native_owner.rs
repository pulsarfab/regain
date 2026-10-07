//! Real production worker transport, explicitly simulated SDK/direct cameras only.
use regain_core::{Exposure, RecoveryOptions, Runtime, Selection};
use regain_hub::{
    activity::ActivityCounter,
    camera::{
        image::{IMAGE_CHUNK_BYTES, ImageBudget, NATIVE_METADATA_BYTES},
        native_owner::{NativeCamera, NativeOperationKind},
        properties::{CameraProperty as P, CameraSetting as S, CameraValue as V},
    },
    source::ErrorKind,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

fn exposure(microseconds: u64) -> Exposure {
    Exposure {
        width: 64,
        height: 64,
        bin: 1,
        x: 0,
        y: 0,
        microseconds,
        dark: true,
    }
}
fn camera(
    direct: bool,
    simulation: Value,
    maximum: usize,
) -> (
    Arc<NativeCamera>,
    ImageBudget,
    ActivityCounter,
    Arc<Mutex<Vec<String>>>,
) {
    camera_model(direct, simulation, maximum, "ZWO ASI676MC")
}
fn camera_model(
    direct: bool,
    simulation: Value,
    maximum: usize,
    direct_model: &str,
) -> (
    Arc<NativeCamera>,
    ImageBudget,
    ActivityCounter,
    Arc<Mutex<Vec<String>>>,
) {
    let budget = ImageBudget::new(maximum).unwrap();
    let activity = ActivityCounter::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let log = events.clone();
    let owner = NativeCamera::new(
        Selection {
            name: if direct {
                direct_model
            } else {
                "ZWO Simulated"
            }
            .into(),
            serial: None,
            direct,
            sdk_fallback: false,
            recovery: RecoveryOptions {
                max_retries: 0,
                ready_frame_download_retries: 0,
                reconnect_delay_seconds: 0.01,
                ..RecoveryOptions::default()
            },
        },
        Runtime {
            directory: std::env::var_os("REGAIN_TEST_WORKERS")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug")
                }),
            sdk: "unused".into(),
            simulate: true,
            sdk_simulation: Some(simulation),
        },
        budget.clone(),
        activity.clone(),
        Arc::new(move |_, event, message| log.lock().unwrap().push(format!("{event}: {message}"))),
    )
    .unwrap();
    (owner, budget, activity, events)
}
fn admission() -> usize {
    exposure(10_000).bytes().unwrap() * 2 + NATIVE_METADATA_BYTES + IMAGE_CHUNK_BYTES
}
async fn until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn settled(owner: &NativeCamera, activity: &ActivityCounter) {
    until(|| {
        let snapshot = owner.snapshot();
        snapshot.operation.is_none() && snapshot.cooling.is_none() && activity.active() == 0
    })
    .await;
}

#[tokio::test]
async fn native_observation_reads_preserve_evidence_times_and_settings_touch_only_their_control() {
    for direct in [false, true] {
        let (owner, _, activity, events) = camera_model(
            direct,
            json!({"instant":true}),
            admission(),
            "ZWO ASI585MM Pro",
        );
        owner.connect().await.unwrap();
        let before = owner.snapshot().core.observations;
        let event_count = events.lock().unwrap().len();
        for _ in 0..2 {
            for (property, kind) in [
                (P::Gain, 0),
                (P::Offset, 5),
                (P::CcdTemperature, 8),
                (P::CoolerPower, 15),
                (P::SetCcdTemperature, 16),
                (P::CoolerOn, 17),
            ] {
                let reading = owner.read_property_observation(property).unwrap();
                assert_eq!(reading.observed_at, Some(before[&kind].observed_at));
                assert_eq!(reading.value, owner.read_property(property).unwrap());
            }
        }
        assert_eq!(events.lock().unwrap().len(), event_count);
        assert_eq!(activity.active(), 0);
        assert!(
            owner
                .read_property_observation(P::CameraXSize)
                .unwrap()
                .observed_at
                .is_none()
        );
        owner.set_imaging_control(0, 123).await.unwrap();
        let after = owner.snapshot().core.observations;
        assert_eq!(after[&0].value, 123);
        assert!(after[&0].observed_at >= before[&0].observed_at);
        for kind in [5, 8, 15, 16, 17] {
            assert_eq!(after[&kind], before[&kind]);
        }
        owner.close().await.unwrap();
        assert_eq!(
            owner.read_property_observation(P::Gain).unwrap_err().kind,
            ErrorKind::Disconnected
        );
    }
}

#[tokio::test]
async fn sdk_and_direct_native_properties_use_shared_types_and_frozen_completed_timing() {
    for direct in [false, true] {
        let (owner, _, activity, _) = camera_model(
            direct,
            json!({"instant":true}),
            admission() * 2,
            "ZWO ASI585MM Pro",
        );
        assert_eq!(
            owner.read_property(P::Gain).unwrap_err().kind,
            ErrorKind::Disconnected
        );
        owner.connect().await.unwrap();
        assert!(owner.snapshot().geometry.is_some());
        for &property in P::ALL {
            let reading = owner.read_property(property);
            let optional = matches!(
                property,
                P::ElectronsPerAdu
                    | P::FastReadout
                    | P::FullWellCapacity
                    | P::Gains
                    | P::HeatSinkTemperature
                    | P::IsPulseGuiding
                    | P::Offsets
                    | P::PercentCompleted
                    | P::SubExposureDuration
            ) || (direct && matches!(property, P::BayerOffsetX | P::BayerOffsetY));
            if optional {
                assert_eq!(
                    reading.unwrap_err().kind,
                    ErrorKind::Unsupported,
                    "{property:?}"
                );
            } else if matches!(property, P::LastExposureDuration | P::LastExposureStartTime) {
                assert_eq!(
                    reading.unwrap_err().kind,
                    ErrorKind::Unavailable,
                    "{property:?}"
                );
            } else {
                assert!(reading.is_ok(), "{property:?}: {reading:?}");
            }
        }
        assert_eq!(
            owner.read_property(P::ImageReady).unwrap(),
            V::Boolean { value: false }
        );
        assert_eq!(
            owner.read_property(P::CanSetCcdTemperature).unwrap(),
            V::Boolean { value: true }
        );
        assert_eq!(
            owner.read_property(P::CanStopExposure).unwrap(),
            V::Boolean { value: false }
        );
        assert_eq!(
            owner.read_property(P::MaxAdu).unwrap(),
            V::Integer { value: 65535 }
        );
        assert_eq!(
            owner.read_property(P::ReadoutModes).unwrap(),
            V::Strings {
                value: vec!["RAW16".into()]
            }
        );
        let id = owner.start(exposure(10_000)).unwrap();
        let reader = owner.wait(id).await.unwrap();
        settled(&owner, &activity).await;
        assert_eq!(
            owner.read_property(P::NumX).unwrap(),
            V::Integer { value: 64 }
        );
        assert_eq!(
            owner.read_property(P::ImageReady).unwrap(),
            V::Boolean { value: true }
        );
        assert_eq!(
            owner.read_property(P::LastExposureDuration).unwrap(),
            V::Number { value: 0.01 }
        );
        let metadata: Value =
            serde_json::from_slice(reader.native().unwrap().metadata_json()).unwrap();
        let V::Text { value } = owner.read_property(P::LastExposureStartTime).unwrap() else {
            panic!("Native exposure timestamp must be text");
        };
        assert!(!value.ends_with('Z') && !value.ends_with("+00:00"));
        let property_time = chrono::NaiveDateTime::parse_from_str(&value, "%Y-%m-%dT%H:%M:%S%.f")
            .unwrap()
            .and_utc();
        let metadata_time =
            chrono::DateTime::parse_from_rfc3339(metadata["startedUtc"].as_str().unwrap())
                .unwrap()
                .with_timezone(&chrono::Utc);
        assert_eq!(property_time, metadata_time);
        let timing = owner.read_property(P::LastExposureStartTime).unwrap();
        owner.configure_geometry(S::NumX(72)).unwrap();
        assert_eq!(
            owner.read_property(P::NumX).unwrap(),
            V::Integer { value: 72 }
        );
        assert_eq!(
            owner.read_property(P::LastExposureStartTime).unwrap(),
            timing
        );
        assert_eq!(reader.native().unwrap().exposure().width, 64);
        owner.close().await.unwrap();
        settled(&owner, &activity).await;
        assert!(owner.snapshot().geometry.is_none());
    }
}

#[tokio::test]
async fn uncooled_camera_reports_missing_cooling_without_fabricated_values() {
    let (owner, _, activity, _) = camera(true, json!({"instant":true}), admission() * 2);
    owner.connect().await.unwrap();
    assert_eq!(
        owner.read_property(P::CanSetCcdTemperature).unwrap(),
        V::Boolean { value: false }
    );
    assert_eq!(
        owner.read_property(P::CanGetCoolerPower).unwrap(),
        V::Boolean { value: false }
    );
    for property in [P::CoolerOn, P::CoolerPower, P::SetCcdTemperature] {
        assert_eq!(
            owner.read_property(property).unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
}

#[tokio::test]
async fn imaging_settings_acknowledge_on_one_owner_and_preserve_previous_frame() {
    for direct in [false, true] {
        let (owner, budget, activity, _) = camera(direct, json!({"instant":true}), admission() * 3);
        owner.connect().await.unwrap();
        let id = owner.start(exposure(10_000)).unwrap();
        let reader = owner.wait(id).await.unwrap();
        settled(&owner, &activity).await;
        let metadata = reader.native().unwrap().metadata_json().to_vec();
        let timing = owner.read_property(P::LastExposureStartTime).unwrap();
        let mut setter = Box::pin(owner.set_imaging_control(0, 123));
        assert!(futures_util::poll!(setter.as_mut()).is_pending());
        assert_eq!(
            owner.snapshot().operation.unwrap().kind,
            NativeOperationKind::Configuring
        );
        assert_eq!(activity.active(), 1);
        assert_eq!(
            owner.start(exposure(10_000)).unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(
            owner.configure_geometry(S::NumX(72)).unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(
            owner.set_imaging_control(5, 20).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(
            owner.set_cooling(16, -15).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(owner.abort().await.unwrap_err().kind, ErrorKind::Busy);
        assert_eq!(
            owner.read_property(P::ImageReady).unwrap(),
            V::Boolean { value: false }
        );
        let mut old_waiter = Box::pin(owner.wait(id));
        assert!(futures_util::poll!(old_waiter.as_mut()).is_pending());
        assert_eq!(setter.await.unwrap(), 123);
        let old_image = old_waiter.await.unwrap();
        settled(&owner, &activity).await;
        assert_eq!(old_image.bytes().as_ptr(), reader.bytes().as_ptr());
        assert_eq!(reader.native().unwrap().metadata_json(), metadata);
        assert_eq!(
            owner.read_property(P::LastExposureStartTime).unwrap(),
            timing
        );
        assert_eq!(
            owner.read_property(P::Gain).unwrap(),
            V::Integer { value: 123 }
        );
        assert_eq!(owner.set_imaging_control(5, 20).await.unwrap(), 20);
        assert_eq!(
            owner.read_property(P::Offset).unwrap(),
            V::Integer { value: 20 }
        );
        assert_eq!(
            owner.set_imaging_control(0, 601).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            owner.set_imaging_control(16, -15).await.unwrap_err().kind,
            ErrorKind::Unsupported
        );
        assert_eq!(activity.active(), 0);
        let next = owner.start(exposure(10_000)).unwrap();
        let frame = owner.wait(next).await.unwrap();
        let latest: Value =
            serde_json::from_slice(frame.native().unwrap().metadata_json()).unwrap();
        assert_eq!(latest["controls"]["0"], 123);
        assert_eq!(latest["controls"]["5"], 20);
        owner.close().await.unwrap();
        settled(&owner, &activity).await;
        drop(frame);
        drop(old_image);
        drop(reader);
        assert_eq!(budget.used_bytes(), 0);
    }
}

#[tokio::test]
async fn unknown_imaging_setting_fences_all_work_until_explicit_reset() {
    let (owner, budget, activity, _) = camera(
        false,
        json!({"instant":true,"clampControl":0,"clampMinimum":100}),
        admission() * 2,
    );
    owner.connect().await.unwrap();
    let id = owner.start(exposure(10_000)).unwrap();
    let reader = owner.wait(id).await.unwrap();
    settled(&owner, &activity).await;
    assert_eq!(
        owner.set_imaging_control(0, 50).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    settled(&owner, &activity).await;
    assert_eq!(
        owner.read_property(P::Gain).unwrap(),
        V::Integer { value: 100 }
    );
    assert_eq!(owner.image().err().unwrap().kind, ErrorKind::Uncertain);
    assert_eq!(
        owner.start(exposure(10_000)).unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.set_imaging_control(0, 150).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.set_cooling(16, -15).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.connect().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(reader.bytes().len(), 8192);
    owner.reset();
    settled(&owner, &activity).await;
    owner.connect().await.unwrap();
    assert_eq!(owner.set_imaging_control(0, 150).await.unwrap(), 150);
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
    drop(reader);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn buffered_setting_ack_cannot_cross_reset_and_capture_rejects_imaging_writes() {
    let (owner, _, activity, _) = camera(false, json!({"instant":false}), admission() * 2);
    owner.connect().await.unwrap();
    let mut setter = Box::pin(owner.set_imaging_control(0, 123));
    assert!(futures_util::poll!(setter.as_mut()).is_pending());
    until(|| owner.snapshot().operation.is_none() && activity.active() == 0).await;
    assert_eq!(
        owner.read_property(P::Gain).unwrap(),
        V::Integer { value: 123 }
    );
    owner.reset();
    assert_eq!(setter.await.unwrap_err().kind, ErrorKind::Disconnected);
    settled(&owner, &activity).await;
    owner.connect().await.unwrap();
    let id = owner.start(exposure(2_000_000)).unwrap();
    until(|| owner.snapshot().core.phase == "Exposing").await;
    assert_eq!(
        owner.set_imaging_control(0, 150).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    owner.abort().await.unwrap();
    settled(&owner, &activity).await;
    assert!(owner.wait(id).await.is_err());
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
}

#[tokio::test]
async fn symmetric_native_geometry_freezes_at_admission_and_rejects_invalid_capture_without_losing_image()
 {
    let (owner, _, activity, _) = camera(false, json!({"instant":false}), admission() * 2);
    owner.connect().await.unwrap();
    owner.configure_geometry(S::BinX(2)).unwrap();
    assert_eq!(
        owner.read_property(P::BinY).unwrap(),
        V::Integer { value: 2 }
    );
    assert_eq!(
        owner.read_property(P::NumX).unwrap(),
        V::Integer { value: 960 },
        "Binning does not silently replace desired ROI"
    );
    assert_eq!(
        owner.start_configured(10_000, true).unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(activity.active(), 0);
    for setting in [
        S::NumX(64),
        S::NumY(64),
        S::StartX(3),
        S::StartY(1),
        S::ReadoutMode(0),
    ] {
        owner.configure_geometry(setting).unwrap();
    }
    let before = serde_json::to_value(owner.snapshot().geometry).unwrap();
    for setting in [
        S::BinY(3),
        S::BinX(0),
        S::NumY(-1),
        S::StartY(-1),
        S::ReadoutMode(1),
    ] {
        assert_eq!(
            owner.configure_geometry(setting).unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(
            serde_json::to_value(owner.snapshot().geometry).unwrap(),
            before
        );
    }
    for setting in [
        S::Gain(100),
        S::Offset(10),
        S::FastReadout(false),
        S::SubExposureDuration(0.0),
    ] {
        assert_eq!(
            owner.configure_geometry(setting).unwrap_err().kind,
            ErrorKind::Unsupported,
            "Hardware writes cannot use local geometry admission"
        );
    }
    let id = owner.start_configured(2_000_000, true).unwrap();
    until(|| owner.snapshot().core.phase == "Exposing").await;
    assert_eq!(
        owner.read_property(P::CameraState).unwrap(),
        V::Integer { value: 2 }
    );
    assert_eq!(
        owner.read_property(P::ImageReady).unwrap(),
        V::Boolean { value: false }
    );
    assert_eq!(
        owner
            .read_property(P::LastExposureDuration)
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(
        owner.configure_geometry(S::NumX(72)).unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        serde_json::to_value(owner.snapshot().geometry).unwrap(),
        before
    );
    assert_eq!(owner.set_cooling(16, -15).await.unwrap(), -15);
    assert_eq!(
        owner.read_property(P::SetCcdTemperature).unwrap(),
        V::Number { value: -15.0 }
    );
    let image = owner.wait(id).await.unwrap();
    settled(&owner, &activity).await;
    assert_eq!(
        image.native().unwrap().exposure(),
        &Exposure {
            bin: 2,
            x: 3,
            y: 1,
            microseconds: 2_000_000,
            ..exposure(10_000)
        }
    );
    owner.configure_geometry(S::NumX(65)).unwrap(); // Individual setter is valid; combined RAW16 alignment is not.
    assert_eq!(
        owner.start_configured(10_000, true).unwrap_err().kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(
        owner.image().unwrap().bytes().as_ptr(),
        image.bytes().as_ptr()
    );
    assert_eq!(
        owner.read_property(P::LastExposureDuration).unwrap(),
        V::Number { value: 2.0 }
    );
    assert_eq!(
        owner.read_property(P::CameraState).unwrap(),
        V::Integer { value: 0 }
    );
    // Positive scalar ROI values are desired state, including combinations that
    // cannot fit. Admission rejects them before allocation/dispatch and retains
    // the completed image. Reconfiguring the valid ROI remains possible.
    for setting in [
        S::NumX(481),
        S::StartX(480),
        S::NumY(i32::MAX),
        S::StartY(i32::MAX),
    ] {
        owner.configure_geometry(S::NumX(64)).unwrap();
        owner.configure_geometry(S::NumY(64)).unwrap();
        owner.configure_geometry(S::StartX(3)).unwrap();
        owner.configure_geometry(S::StartY(1)).unwrap();
        owner.configure_geometry(setting).unwrap();
        assert_eq!(
            owner.start_configured(10_000, true).unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert_eq!(activity.active(), 0);
        assert_eq!(
            owner.image().unwrap().bytes().as_ptr(),
            image.bytes().as_ptr()
        );
    }
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
}

#[tokio::test]
async fn malformed_native_initial_geometry_fails_connection_without_exposure_or_owned_leaks() {
    for simulation in [
        json!({"width":7}),
        json!({"height":1}),
        json!({"width":-1}),
        json!({"width":2147483648i64}),
        json!({"bins":[]}),
        json!({"bins":[0]}),
        json!({"bins":["1"]}),
    ] {
        let (owner, budget, activity, events) = camera(false, simulation, admission() * 2);
        assert_eq!(
            owner.connect().await.unwrap_err().kind,
            ErrorKind::Unavailable
        );
        settled(&owner, &activity).await;
        assert!(!owner.snapshot().connected);
        assert!(owner.snapshot().geometry.is_none());
        assert_eq!(budget.used_bytes(), 0);
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("Starting exposure"))
        );
        owner.close().await.unwrap();
        settled(&owner, &activity).await;
    }
}

#[tokio::test]
async fn idle_cooling_validates_and_admits_once_and_caller_loss_skips_unsent_work() {
    for direct in [false, true] {
        let (owner, _, activity, _) = camera_model(
            direct,
            json!({"instant":true}),
            admission() * 2,
            "ZWO ASI585MM Pro",
        );
        assert_eq!(
            owner.set_cooling(16, -10).await.unwrap_err().kind,
            ErrorKind::Disconnected
        );
        owner.connect().await.unwrap();
        for (control, value) in [(0, 100), (16, -100), (17, 2)] {
            assert_eq!(
                owner.set_cooling(control, value).await.unwrap_err().kind,
                ErrorKind::InvalidValue
            );
            assert_eq!(activity.active(), 0);
            assert!(owner.snapshot().cooling.is_none());
        }
        let original = owner.snapshot().core.values[&16];
        let mut setter = Box::pin(owner.set_cooling(16, -20));
        // This current-thread test has not yielded to the retained task yet.
        assert!(futures_util::poll!(setter.as_mut()).is_pending());
        assert_eq!(activity.active(), 1);
        let pending = owner.snapshot().cooling.unwrap();
        assert_eq!((pending.control, pending.value), (16, -20));
        assert_eq!(
            owner.set_cooling(17, 1).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(
            owner.start(exposure(10_000)).unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(owner.abort().await.unwrap_err().kind, ErrorKind::Busy);
        drop(setter);
        settled(&owner, &activity).await;
        assert_eq!(owner.snapshot().core.values[&16], original);
        assert!(owner.snapshot().error.is_none());
        for (control, value) in [(16, -10), (17, 1), (16, -15)] {
            assert_eq!(owner.set_cooling(control, value).await.unwrap(), value);
            assert_eq!(owner.snapshot().core.values[&control], value);
            assert_eq!(activity.active(), 0);
        }
        owner.close().await.unwrap();
        settled(&owner, &activity).await;
    }
}

#[tokio::test]
async fn completed_image_publication_waits_for_known_cooling_and_preserves_readers() {
    let (owner, budget, activity, _) = camera(false, json!({"instant":true}), admission() * 2);
    owner.connect().await.unwrap();
    let id = owner.start(exposure(10_000)).unwrap();
    let reader = owner.wait(id).await.unwrap();
    settled(&owner, &activity).await;
    let mut setter = Box::pin(owner.set_cooling(16, -15));
    assert!(futures_util::poll!(setter.as_mut()).is_pending());
    assert!(!owner.snapshot().image_ready);
    assert_eq!(owner.image().err().unwrap().kind, ErrorKind::Busy);
    let mut waiter = Box::pin(owner.wait(id));
    assert!(futures_util::poll!(waiter.as_mut()).is_pending());
    setter.await.unwrap();
    let image = waiter.await.unwrap();
    assert_eq!(image.bytes().as_ptr(), reader.bytes().as_ptr());
    assert!(owner.snapshot().image_ready);
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
    assert_eq!(reader.bytes().len(), 8192);
    drop(reader);
    drop(image);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn active_sdk_and_direct_capture_acknowledge_cooling_without_engine_deadlock() {
    for direct in [false, true] {
        let (owner, _, activity, _) = camera_model(
            direct,
            json!({"instant":false}),
            admission() * 2,
            "ZWO ASI585MM Pro",
        );
        owner.connect().await.unwrap();
        let id = owner.start(exposure(6_000_000)).unwrap();
        until(|| owner.snapshot().core.phase == "Exposing").await;
        for (control, value) in [(16, -15), (17, 1), (16, -20)] {
            assert_eq!(owner.set_cooling(control, value).await.unwrap(), value);
            assert_eq!(owner.snapshot().core.values[&control], value);
            assert_eq!(
                activity.active(),
                1,
                "Capture retains activity after cooler acknowledgement"
            );
        }
        let image = owner.wait(id).await.unwrap();
        settled(&owner, &activity).await;
        let metadata: Value =
            serde_json::from_slice(image.native().unwrap().metadata_json()).unwrap();
        assert_eq!(metadata["controls"]["16"], -20);
        assert_eq!(metadata["controls"]["17"], 1);
        assert_eq!(
            image.bytes(),
            (0..4096u16).flat_map(u16::to_le_bytes).collect::<Vec<_>>()
        );
        owner.close().await.unwrap();
        settled(&owner, &activity).await;
    }
}

#[tokio::test]
async fn reset_retires_queued_cooling_and_cannot_acknowledge_a_later_generation() {
    let (owner, _, activity, _) = camera(false, json!({"instant":true}), admission() * 2);
    owner.connect().await.unwrap();
    let generation = owner.snapshot().generation;
    let mut setter = Box::pin(owner.set_cooling(16, -20));
    assert!(futures_util::poll!(setter.as_mut()).is_pending());
    owner.reset();
    assert_ne!(owner.snapshot().generation, generation);
    assert!(owner.snapshot().cooling.is_none());
    assert_eq!(setter.await.unwrap_err().kind, ErrorKind::Disconnected);
    settled(&owner, &activity).await;
    owner.connect().await.unwrap();
    assert_eq!(owner.set_cooling(16, -15).await.unwrap(), -15);
    assert_eq!(owner.snapshot().core.values[&16], -15);
    let mut acknowledged = Box::pin(owner.set_cooling(16, -18));
    assert!(futures_util::poll!(acknowledged.as_mut()).is_pending());
    // Let the retained task acknowledge, but keep its caller unpolled. A reset
    // must reject even this already-buffered success for the retired generation.
    settled(&owner, &activity).await;
    assert_eq!(owner.snapshot().core.values[&16], -18);
    owner.reset();
    assert_eq!(
        acknowledged.await.unwrap_err().kind,
        ErrorKind::Disconnected
    );
    settled(&owner, &activity).await;
    owner.connect().await.unwrap();
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
}

#[tokio::test]
async fn uncertain_idle_cooler_blocks_publication_and_restarts_until_explicit_reset() {
    let (owner, budget, activity, _) = camera(
        false,
        json!({"instant":true,"clampControl":16,"clampMinimum":-10}),
        admission() * 2,
    );
    owner.connect().await.unwrap();
    // Admit a target the clamp can acknowledge before initial capture settings
    // are applied; only the later negative request should fail readback.
    owner.set_cooling(16, 0).await.unwrap();
    let id = owner.start(exposure(10_000)).unwrap();
    let reader = owner.wait(id).await.unwrap();
    settled(&owner, &activity).await;
    let original = owner.snapshot().core.values[&16];
    let error = owner.set_cooling(16, -15).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Uncertain);
    assert!(
        !error.message.contains("readback"),
        "Worker details stay redacted"
    );
    assert_eq!(owner.snapshot().core.values[&16], original);
    assert!(!owner.snapshot().core.control_connection_available);
    assert!(!owner.snapshot().image_ready);
    assert_eq!(
        owner.wait(id).await.err().unwrap().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(owner.image().err().unwrap().kind, ErrorKind::Uncertain);
    assert_eq!(
        owner.read_property(P::ImageReady).unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.read_property(P::CameraState).unwrap(),
        V::Integer { value: 5 }
    );
    assert_eq!(
        owner.configure_geometry(S::NumX(64)).unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.start(exposure(10_000)).unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.connect().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.set_cooling(17, 0).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(owner.abort().await.unwrap_err().kind, ErrorKind::Uncertain);
    assert_eq!(reader.bytes().len(), 8192);
    owner.reset();
    settled(&owner, &activity).await;
    owner.connect().await.unwrap();
    assert_eq!(owner.set_cooling(16, 5).await.unwrap(), 5);
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
    drop(reader);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn uncertain_capture_cooler_retains_cleanup_and_never_publishes_or_retries() {
    let (owner, budget, activity, events) = camera(
        false,
        json!({"instant":false,"clampControl":16,"clampMinimum":-10}),
        admission() * 2,
    );
    owner.connect().await.unwrap();
    owner.set_cooling(16, 0).await.unwrap();
    let id = owner.start(exposure(6_000_000)).unwrap();
    until(|| owner.snapshot().core.phase == "Exposing").await;
    assert_eq!(
        owner.set_cooling(16, -15).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        owner.wait(id).await.err().unwrap().kind,
        ErrorKind::Uncertain
    );
    settled(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 0);
    assert!(!owner.snapshot().image_ready);
    assert_eq!(
        owner.start(exposure(10_000)).unwrap_err().kind,
        ErrorKind::Uncertain
    );
    let lines = events.lock().unwrap().clone();
    assert!(!lines.iter().any(|line| line.starts_with("capture.retry:")));
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("session.phase: Starting exposure"))
            .count(),
        1
    );
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
}

#[tokio::test]
async fn abandoned_connection_waiter_and_concurrent_clients_share_one_handshake() {
    let (owner, _, activity, events) = camera(false, json!({"instant":true}), admission() * 2);
    let mut first = Box::pin(owner.connect());
    assert!(futures_util::poll!(first.as_mut()).is_pending());
    assert_eq!(activity.active(), 1);
    assert_eq!(
        owner.snapshot().operation.unwrap().kind,
        NativeOperationKind::Connecting
    );
    drop(first);
    let (left, right) = tokio::join!(owner.connect(), owner.connect());
    left.unwrap();
    right.unwrap();
    assert!(owner.snapshot().connected);
    settled(&owner, &activity).await;
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with("connection.opened:"))
            .count(),
        1
    );
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
    assert!(!owner.snapshot().connected);
}

#[tokio::test]
async fn dropped_capture_waiter_retains_activity_status_and_one_immutable_image() {
    let (owner, budget, activity, events) =
        camera(false, json!({"instant":false}), admission() * 2);
    owner.connect().await.unwrap();
    let id = owner.start(exposure(6_000_000)).unwrap();
    assert_eq!(activity.active(), 1);
    let mut waiter = Box::pin(owner.wait(id));
    assert!(futures_util::poll!(waiter.as_mut()).is_pending());
    drop(waiter);
    until(|| owner.snapshot().core.phase == "Exposing").await;
    let status = owner.snapshot();
    assert!(status.connected);
    assert!(!status.image_ready);
    assert!(status.core.values.contains_key(&8));
    assert!(status.core.values.contains_key(&15));
    assert_eq!(
        owner.start(exposure(10_000)).unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(budget.used_bytes(), admission());
    let image = owner.wait(id).await.unwrap();
    settled(&owner, &activity).await;
    let reader = owner.image().unwrap();
    assert_eq!(image.bytes().as_ptr(), reader.bytes().as_ptr());
    assert!(owner.snapshot().image_ready);
    let metadata: Value = serde_json::from_slice(image.native().unwrap().metadata_json()).unwrap();
    assert_eq!(metadata["backend"], "sdk");
    assert_eq!(metadata["recoveries"], 0);
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("session.phase: Aborted"))
    );
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 8192 + NATIVE_METADATA_BYTES);
    assert_eq!(reader.bytes().len(), 8192);
    drop(image);
    drop(reader);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn rejected_admission_and_invalid_geometry_preserve_completed_image() {
    let (owner, budget, activity, events) = camera(false, json!({"instant":true}), admission());
    owner.connect().await.unwrap();
    let id = owner.start(exposure(10_000)).unwrap();
    let image = owner.wait(id).await.unwrap();
    settled(&owner, &activity).await;
    let before = events.lock().unwrap().clone();
    assert_eq!(
        owner.start(exposure(10_000)).unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        owner
            .start(Exposure {
                x: u32::MAX,
                ..exposure(10_000)
            })
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    assert_eq!(*events.lock().unwrap(), before);
    assert_eq!(
        owner.image().unwrap().bytes().as_ptr(),
        image.bytes().as_ptr()
    );
    owner.abort().await.unwrap();
    assert!(owner.snapshot().image_ready, "Idle abort is inert");
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
    drop(image);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn explicit_abort_discards_pixels_after_core_cleanup_and_allows_next_capture() {
    for direct in [false, true] {
        let (owner, budget, activity, _) =
            camera(direct, json!({"instant":false}), admission() * 2);
        owner.connect().await.unwrap();
        let id = owner.start(exposure(2_000_000)).unwrap();
        until(|| owner.snapshot().core.phase == "Exposing").await;
        let mut abort = Box::pin(owner.abort());
        assert!(futures_util::poll!(abort.as_mut()).is_pending());
        assert_eq!(
            owner.snapshot().operation.unwrap().kind,
            NativeOperationKind::Aborting
        );
        drop(abort); // Cancellation is already committed, not owned by this waiter.
        settled(&owner, &activity).await;
        assert!(!owner.snapshot().image_ready);
        assert_eq!(budget.used_bytes(), 0);
        assert!(owner.wait(id).await.is_err());
        let next = owner.start(exposure(10_000)).unwrap();
        assert_ne!(next, id);
        let image = owner.wait(next).await.unwrap();
        assert_eq!(image.bytes().len(), 8192);
        drop(image);
        owner.close().await.unwrap();
        settled(&owner, &activity).await;
        assert_eq!(budget.used_bytes(), 0);
    }
}

#[tokio::test]
async fn reset_fences_capture_and_cleanup_before_a_fresh_connection() {
    let (owner, budget, activity, _) = camera(false, json!({"instant":false}), admission() * 2);
    owner.connect().await.unwrap();
    let generation = owner.snapshot().generation;
    let id = owner.start(exposure(2_000_000)).unwrap();
    until(|| owner.snapshot().core.phase == "Exposing").await;
    let mut waiter = Box::pin(owner.wait(id));
    assert!(futures_util::poll!(waiter.as_mut()).is_pending());
    owner.reset();
    assert_ne!(owner.snapshot().generation, generation);
    assert_eq!(waiter.await.err().unwrap().kind, ErrorKind::Disconnected);
    assert!(!owner.snapshot().connected);
    assert!(!owner.snapshot().image_ready);
    assert_eq!(owner.connect().await.unwrap_err().kind, ErrorKind::Busy);
    settled(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 0);
    owner.connect().await.unwrap();
    assert_ne!(owner.snapshot().generation, generation);
    let next = owner.start(exposure(10_000)).unwrap();
    let frame = owner.wait(next).await.unwrap();
    drop(frame);
    assert!(
        owner.wait(id).await.is_err(),
        "Retired acquisition cannot adopt the new frame"
    );
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn reset_before_capture_dispatch_and_abandoned_close_keep_cleanup_owned() {
    let (owner, budget, activity, events) = camera(false, json!({"instant":true}), admission() * 2);
    owner.connect().await.unwrap();
    let before = events.lock().unwrap().len();
    let id = owner.start(exposure(10_000)).unwrap();
    owner.reset();
    owner.reset();
    settled(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 0);
    assert!(owner.wait(id).await.is_err());
    assert!(
        !events.lock().unwrap()[before..]
            .iter()
            .any(|line| line.contains("Starting exposure"))
    );
    owner.connect().await.unwrap();
    let mut close = Box::pin(owner.close());
    assert!(futures_util::poll!(close.as_mut()).is_pending());
    drop(close);
    assert!(activity.active() > 0);
    settled(&owner, &activity).await;
    assert!(!owner.snapshot().connected);
}

#[tokio::test]
async fn core_failure_clears_owned_work_and_budget_without_adding_retries() {
    let (owner, budget, activity, events) = camera(
        false,
        json!({"instant":true,"fault":"download"}),
        admission() * 2,
    );
    owner.connect().await.unwrap();
    let id = owner.start(exposure(10_000)).unwrap();
    let error = owner.wait(id).await.err().unwrap();
    assert_eq!(error.kind, ErrorKind::Unavailable);
    assert_eq!(error.upstream_code, Some(11));
    assert!(
        !error.message.contains("download"),
        "Raw worker diagnostics stay out of source errors"
    );
    assert_eq!(
        owner
            .wait(uuid::Uuid::new_v4())
            .await
            .err()
            .unwrap()
            .message,
        "Native acquisition is no longer current"
    );
    settled(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 0);
    assert!(!owner.snapshot().image_ready);
    assert!(owner.snapshot().error.is_some());
    assert_eq!(owner.image().err().unwrap().kind, ErrorKind::Unavailable);
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains("session.phase: Starting exposure"))
            .count(),
        1
    );
    owner.close().await.unwrap();
    settled(&owner, &activity).await;
}

#[tokio::test]
async fn capture_survives_loss_of_all_external_owner_references() {
    let (owner, budget, activity, events) =
        camera(false, json!({"instant":false}), admission() * 2);
    owner.connect().await.unwrap();
    owner.start(exposure(6_000_000)).unwrap();
    until(|| owner.snapshot().core.phase == "Exposing").await;
    let retained = Arc::downgrade(&owner);
    drop(owner);
    assert!(retained.upgrade().is_some());
    assert_eq!(activity.active(), 1);
    until(|| activity.active() == 0).await;
    assert!(retained.upgrade().is_none());
    assert_eq!(budget.used_bytes(), 0);
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|line| line.contains("session.phase: Downloading"))
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|line| line.contains("session.phase: Aborted"))
    );
}
