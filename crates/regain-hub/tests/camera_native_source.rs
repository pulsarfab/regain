//! Production source actor/supervisor and worker pipes, explicit simulation only.
use regain_core::{RecoveryOptions, Runtime, Selection};
use regain_hub::{
    activity::ActivityCounter,
    camera::{
        acquisition::{AcquisitionTiming, CameraSupervisor, ExposureRequest},
        image::ImageBudget,
        native_owner::{NativeCamera, NativeOperationKind},
        native_source::NativeCameraBackend,
        properties::{CameraProperty as P, CameraSetting as S, CameraValue as V},
    },
    parameters::PollPolicy,
    safety::MonotonicClock,
    source::{Backend, ErrorKind, SourceHandle, Values},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

type Events = Arc<Mutex<Vec<String>>>;
fn fixture(
    direct: bool,
    simulation: Value,
) -> (Arc<NativeCamera>, ImageBudget, ActivityCounter, Events) {
    fixture_recovery(
        direct,
        simulation,
        RecoveryOptions {
            max_retries: 0,
            ready_frame_download_retries: 0,
            reconnect_delay_seconds: 0.01,
            ..RecoveryOptions::default()
        },
    )
}
fn fixture_recovery(
    direct: bool,
    simulation: Value,
    recovery: RecoveryOptions,
) -> (Arc<NativeCamera>, ImageBudget, ActivityCounter, Events) {
    let budget = ImageBudget::new(16 * 1024 * 1024).unwrap();
    let activity = ActivityCounter::default();
    let events: Events = Arc::default();
    let log = events.clone();
    let owner = NativeCamera::new(
        Selection {
            name: if direct {
                "ZWO ASI585MM Pro"
            } else {
                "ZWO Simulated"
            }
            .into(),
            serial: None,
            direct,
            sdk_fallback: false,
            recovery,
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
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn idle(owner: &NativeCamera, activity: &ActivityCounter) {
    until(|| {
        owner.snapshot().operation.is_none()
            && owner.snapshot().cooling.is_none()
            && activity.active() == 0
    })
    .await;
}
fn source(owner: Arc<NativeCamera>, samples: &[P]) -> Arc<SourceHandle> {
    source_with_timeout(owner, samples, 5.)
}
fn source_with_timeout(owner: Arc<NativeCamera>, samples: &[P], seconds: f64) -> Arc<SourceHandle> {
    SourceHandle::spawn(
        Uuid::new_v4(),
        Uuid::new_v4(),
        PollPolicy {
            poll_seconds: 60.0,
            request_timeout_seconds: seconds,
            ..PollPolicy::default()
        },
        Box::new(
            NativeCameraBackend::new(owner, samples.iter().map(|p| p.sample_request()).collect())
                .unwrap(),
        ),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap()
}
fn supervisor(
    source: Arc<SourceHandle>,
    budget: ImageBudget,
    activity: ActivityCounter,
) -> Arc<CameraSupervisor> {
    CameraSupervisor::new(
        source,
        budget,
        AcquisitionTiming {
            poll_interval: Duration::from_millis(10),
            ..AcquisitionTiming::default()
        },
        activity,
    )
    .unwrap()
}

#[tokio::test]
async fn native_reread_and_post_abort_restoration_outlive_outer_scalar_and_proxy_bounds() {
    let (owner, budget, activity, events) = fixture_recovery(
        false,
        json!({"instant":false,"fault":"download"}),
        RecoveryOptions {
            max_retries: 0,
            ready_frame_download_retries: 1,
            reconnect_delay_seconds: 0.3,
            ..RecoveryOptions::default()
        },
    );
    let source = source_with_timeout(owner.clone(), &[], 0.05);
    let supervisor = CameraSupervisor::new(
        source.clone(),
        budget.clone(),
        AcquisitionTiming {
            readiness_grace: Duration::from_millis(10),
            poll_interval: Duration::from_millis(10),
            ..AcquisitionTiming::default()
        },
        activity.clone(),
    )
    .unwrap();
    let first = supervisor.connect().await.unwrap();
    let second = supervisor.connect().await.unwrap();
    first.set(S::NumX(64)).await.unwrap();
    first.set(S::NumY(64)).await.unwrap();
    let generation = source.snapshot().generation;
    let id = first
        .start(ExposureRequest {
            duration_seconds: 0.01,
            light: false,
        })
        .await
        .unwrap();
    until(|| {
        owner
            .snapshot()
            .core
            .phase
            .starts_with("Rereading ready frame")
    })
    .await;
    until(|| supervisor.status().image_ready).await;
    let a = first.image().unwrap();
    let b = second.image().unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(a.identity.acquisition, id);
    let metadata: Value =
        serde_json::from_slice(a.image.native().unwrap().metadata_json()).unwrap();
    assert_eq!(metadata["downloadRetries"], 1);
    assert_eq!(metadata["recoveries"], 0);
    assert_eq!(source.snapshot().generation, generation);
    assert!(!source.snapshot().write_uncertain);
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.starts_with("connection.opened:"))
            .count(),
        1
    );
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.contains("session.phase: Starting exposure"))
            .count(),
        1
    );
    // A clean abort retains the worker. A later setting must not reconnect or
    // change the source generation, even with a short scalar request bound.
    first
        .start(ExposureRequest {
            duration_seconds: 1.,
            light: false,
        })
        .await
        .unwrap();
    until(|| owner.snapshot().core.phase == "Exposing").await;
    let pid = owner.snapshot().core.process_id;
    first.abort().await.unwrap();
    assert!(owner.snapshot().core.control_connection_available);
    first.set(S::Gain(123)).await.unwrap();
    assert_eq!(owner.snapshot().core.process_id, pid);
    assert_eq!(
        second.property(P::Gain).await.unwrap(),
        V::Integer { value: 123 }
    );
    assert_eq!(source.snapshot().generation, generation);
    assert!(!source.snapshot().write_uncertain);
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.starts_with("capture.retry:"))
    );
    drop(first);
    drop(second);
    drop(supervisor);
    source.shutdown().await.unwrap();
    assert_eq!(activity.active(), 0);
    assert!(owner.snapshot().core.process_id.is_none());
    drop(a);
    drop(b);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn disconnect_before_connection_task_runs_never_opens_a_worker_and_drains_the_waiter() {
    let (owner, _, activity, events) = fixture(false, json!({"instant":true}));
    let mut backend = NativeCameraBackend::new(owner.clone(), vec![]).unwrap();
    // Current-thread execution cannot run the spawned connection waiter until
    // this test yields. Its retirement must already be reserved at admission.
    assert!(!backend.connect_step().await.unwrap());
    assert_eq!(activity.active(), 1);
    assert!(owner.snapshot().operation.is_none());
    backend.disconnect().await.unwrap();
    backend.finish_shutdown().await.unwrap();
    assert_eq!(activity.active(), 0);
    assert!(!owner.snapshot().connected);
    assert!(owner.snapshot().core.process_id.is_none());
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event.starts_with("connection.opened:"))
    );
}

#[tokio::test]
async fn native_source_supervisor_shares_one_capture_and_pinned_image_across_clients_and_disconnect()
 {
    for direct in [false, true] {
        let (owner, budget, activity, events) = fixture(direct, json!({"instant":true}));
        let source = source(owner.clone(), &[]);
        let supervisor = supervisor(source.clone(), budget.clone(), activity.clone());
        let first = supervisor.connect().await.unwrap();
        let second = supervisor.connect().await.unwrap();
        assert!(source.snapshot().simulated);
        assert_eq!(source.snapshot().lease_count, 2);
        first.set(S::NumX(64)).await.unwrap();
        first.set(S::NumY(64)).await.unwrap();
        first.set(S::Gain(123)).await.unwrap();
        assert_eq!(
            second.property(P::Gain).await.unwrap(),
            V::Integer { value: 123 }
        );
        let id = first
            .start(ExposureRequest {
                duration_seconds: 0.01,
                light: false,
            })
            .await
            .unwrap();
        until(|| supervisor.status().image_ready).await;
        let a = first.image().unwrap();
        let b = second.image().unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.identity.acquisition, id);
        assert_eq!(a.image.descriptor().width(), 64);
        assert_eq!(a.image.descriptor().height(), 64);
        assert!(std::ptr::eq(
            a.image.bytes().as_ptr(),
            owner.image().unwrap().bytes().as_ptr()
        ));
        let metadata: Value =
            serde_json::from_slice(a.image.native().unwrap().metadata_json()).unwrap();
        assert_eq!(metadata["controls"]["0"], 123);
        assert_eq!(metadata["exposure"]["microseconds"], 10_000);
        let retained = budget.used_bytes();
        assert!(retained > 8192);
        drop(first);
        until(|| source.snapshot().lease_count == 1).await;
        assert!(second.connected());
        assert_eq!(second.image().unwrap().identity.acquisition, id);
        drop(second);
        until(|| !owner.snapshot().connected && source.snapshot().lease_count == 0).await;
        source.shutdown().await.unwrap();
        idle(&owner, &activity).await;
        assert_eq!(budget.used_bytes(), retained);
        drop(a);
        drop(b);
        drop(supervisor);
        assert_eq!(budget.used_bytes(), 0);
        assert_eq!(
            events
                .lock()
                .unwrap()
                .iter()
                .filter(|line| line.starts_with("connection.opened:"))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn native_batch_preserves_hardware_ages_optional_errors_and_refreshes_only_environment() {
    let (owner, _, activity, _) = fixture(
        false,
        json!({
        "instant":true, "observationControl":8, "observationReply":{"value":100,"ageSeconds":20.0}
        }),
    );
    let samples = [
        P::Gain,
        P::CcdTemperature,
        P::CoolerPower,
        P::CameraXSize,
        P::Gains,
    ];
    let mut backend = NativeCameraBackend::new(
        owner.clone(),
        samples.into_iter().map(|p| p.sample_request()).collect(),
    )
    .unwrap();
    backend.connect().await.unwrap();
    let before = owner.snapshot().core;
    let batch = backend.sample().await.unwrap();
    assert!(batch.ages_seconds["ccdtemperature"] >= 20.0);
    assert!(batch.ages_seconds.contains_key("gain"));
    assert!(!batch.ages_seconds.contains_key("cameraxsize"));
    assert_eq!(batch.errors["gains"].kind, ErrorKind::Unsupported);
    assert_eq!(batch.values["cameraxsize"], before.info["width"]);
    idle(&owner, &activity).await;
    let after = owner.snapshot().core;
    assert_eq!(after.process_id, before.process_id);
    for kind in [0, 5, 16, 17] {
        assert_eq!(after.observations[&kind], before.observations[&kind]);
    }
    for kind in [8, 15] {
        assert!(after.observations[&kind].observed_at > before.observations[&kind].observed_at);
    }
    backend.disconnect().await.unwrap();
    idle(&owner, &activity).await;
    let source = source(owner.clone(), &samples);
    let lease = Uuid::new_v4();
    source.acquire(lease).await.unwrap();
    until(|| source.snapshot().values.contains_key("ccdtemperature")).await;
    let state = source.snapshot();
    assert!(state.sample_ages_seconds["ccdtemperature"] >= 20.0);
    assert_eq!(state.sample_errors["gains"].kind, ErrorKind::Unsupported);
    assert!(state.values.contains_key("gain"));
    source.release(lease).await.unwrap();
    source.shutdown().await.unwrap();
    idle(&owner, &activity).await;
}

#[tokio::test]
async fn native_commands_reject_malformed_unsupported_and_cross_budget_reads_without_extra_capture()
{
    let (owner, budget, activity, _) = fixture(false, json!({"instant":true}));
    let mut backend = NativeCameraBackend::new(owner.clone(), vec![]).unwrap();
    backend.connect().await.unwrap();
    for (member, parameters, kind) in [
        ("gain", json!({"Gain":1.5}), ErrorKind::InvalidValue),
        (
            "gain",
            json!({"Gain":1,"extra":false}),
            ErrorKind::InvalidValue,
        ),
        ("gain", json!({"gain":1}), ErrorKind::InvalidValue),
        ("cooleron", json!({"CoolerOn":1}), ErrorKind::InvalidValue),
        (
            "setccdtemperature",
            json!({"SetCCDTemperature":-10.5}),
            ErrorKind::InvalidValue,
        ),
        (
            "startexposure",
            json!({"Duration":0,"Light":true}),
            ErrorKind::InvalidValue,
        ),
        (
            "startexposure",
            json!({"Duration":1e30,"Light":true}),
            ErrorKind::InvalidValue,
        ),
        (
            "startexposure",
            json!({"Duration":0.01,"Light":1}),
            ErrorKind::InvalidValue,
        ),
        (
            "startexposure",
            json!({"Duration":0.01,"Light":true,"extra":1}),
            ErrorKind::InvalidValue,
        ),
        ("stopexposure", json!({}), ErrorKind::Unsupported),
        (
            "fastreadout",
            json!({"FastReadout":true}),
            ErrorKind::Unsupported,
        ),
    ] {
        let parameters: Values = serde_json::from_value(parameters).unwrap();
        assert_eq!(
            backend
                .write(member.into(), parameters)
                .await
                .unwrap_err()
                .kind,
            kind,
            "{member}"
        );
    }
    assert!(owner.snapshot().acquisition.is_none());
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(
        backend
            .read("gain".into(), Values::from([("extra".into(), json!(1))]))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    owner.configure_geometry(S::NumX(64)).unwrap();
    owner.configure_geometry(S::NumY(64)).unwrap();
    backend
        .write(
            "startexposure".into(),
            serde_json::from_value(json!({"Duration":0.01,"Light":false})).unwrap(),
        )
        .await
        .unwrap();
    owner
        .wait(owner.snapshot().acquisition.unwrap())
        .await
        .unwrap();
    let charged = budget.used_bytes();
    let other = ImageBudget::new(16 * 1024 * 1024).unwrap();
    assert_eq!(
        backend
            .camera_image(other.clone())
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Permanent
    );
    assert_eq!(other.used_bytes(), 0);
    let image = backend.camera_image(budget.clone()).await.unwrap();
    assert_eq!(budget.used_bytes(), charged);
    backend.disconnect().await.unwrap();
    drop(image);
    idle(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn unknown_initial_setting_fences_actor_reconnect_until_last_lease_disconnect() {
    let (owner, budget, activity, events) = fixture(
        false,
        json!({
            "instant":true,"observationControl":0,"observationReply":{"value":0,"ageSeconds":-1}
        }),
    );
    let source = source(owner.clone(), &[]);
    let keeper = Uuid::new_v4();
    source.acquire(keeper).await.unwrap();
    until(|| {
        source
            .snapshot()
            .error
            .is_some_and(|e| e.kind == ErrorKind::Uncertain)
    })
    .await;
    assert!(source.snapshot().write_uncertain);
    assert!(source.snapshot().connection_info.unwrap().uncertain);
    let supervisor = supervisor(source.clone(), budget, activity.clone());
    assert_eq!(
        supervisor.connect().await.err().unwrap().kind,
        ErrorKind::Uncertain
    );
    for _ in 0..3 {
        assert_eq!(
            source
                .read(keeper, "gain", Values::new())
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Uncertain
        );
    }
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with("connection.opened:"))
            .count(),
        1
    );
    source.release(keeper).await.unwrap();
    until(|| source.snapshot().lease_count == 0).await;
    assert!(!source.snapshot().write_uncertain);
    let next = Uuid::new_v4();
    source.acquire(next).await.unwrap();
    until(|| {
        source
            .snapshot()
            .error
            .is_some_and(|e| e.kind == ErrorKind::Uncertain)
    })
    .await;
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with("connection.opened:"))
            .count(),
        2
    );
    source.release(next).await.unwrap();
    source.shutdown().await.unwrap();
    idle(&owner, &activity).await;
}

#[tokio::test]
async fn retained_refresh_skips_capture_and_reset_prevents_stale_publication() {
    let (owner, budget, activity, _) = fixture(false, json!({"instant":false}));
    owner.connect().await.unwrap();
    owner.configure_geometry(S::NumX(64)).unwrap();
    owner.configure_geometry(S::NumY(64)).unwrap();
    let id = owner.start_configured(5_000_000, true).unwrap();
    assert!(!owner.begin_environment_refresh().unwrap());
    owner.abort().await.unwrap();
    assert!(owner.wait(id).await.is_err());
    idle(&owner, &activity).await;
    // Aborted capture records an acquisition error; reconnect deliberately before
    // testing an independent refresh/reset generation.
    owner.close().await.unwrap();
    owner.connect().await.unwrap();
    assert!(owner.begin_environment_refresh().unwrap());
    assert_eq!(
        owner.snapshot().operation.unwrap().kind,
        NativeOperationKind::Refreshing
    );
    assert_eq!(activity.active(), 1);
    owner.reset();
    let generation = owner.snapshot().generation;
    idle(&owner, &activity).await;
    assert_eq!(owner.snapshot().generation, generation);
    assert!(!owner.snapshot().connected);
    assert_eq!(budget.used_bytes(), 0);
    owner.connect().await.unwrap();
    assert!(owner.begin_environment_refresh().unwrap());
    idle(&owner, &activity).await;
    assert!(owner.snapshot().connected);
    owner.close().await.unwrap();
}

#[tokio::test]
async fn reset_during_initialization_is_fenced_until_explicit_disconnect() {
    let (owner, _, activity, events) = fixture(false, json!({"instant":true}));
    let mut backend = NativeCameraBackend::new(owner.clone(), vec![]).unwrap();
    assert!(!backend.connect_step().await.unwrap());
    backend.reset();
    idle(&owner, &activity).await;
    assert_eq!(
        backend.connect_step().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert!(backend.connection_info().unwrap().uncertain);
    assert!(
        !events
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.starts_with("connection.opened:"))
    );
    backend.disconnect().await.unwrap();
    backend.connect().await.unwrap();
    assert!(owner.snapshot().connected);
    assert!(!backend.connection_info().unwrap().uncertain);
    backend.disconnect().await.unwrap();
    idle(&owner, &activity).await;
}

#[tokio::test]
async fn telemetry_refresh_preserves_published_image_and_commands_wait_before_dispatch() {
    let (owner, budget, activity, _) = fixture(false, json!({"instant":true}));
    let mut backend =
        NativeCameraBackend::new(owner.clone(), vec![P::CcdTemperature.sample_request()]).unwrap();
    backend.connect().await.unwrap();
    owner.configure_geometry(S::NumX(64)).unwrap();
    owner.configure_geometry(S::NumY(64)).unwrap();
    let id = owner.start_configured(10_000, true).unwrap();
    let reader = owner.wait(id).await.unwrap();
    idle(&owner, &activity).await;
    let charged = budget.used_bytes();
    backend.sample().await.unwrap();
    assert_eq!(
        owner.snapshot().operation.unwrap().kind,
        NativeOperationKind::Refreshing
    );
    assert!(owner.snapshot().image_ready);
    assert_eq!(
        owner.read_property(P::ImageReady).unwrap(),
        V::Boolean { value: true }
    );
    assert_eq!(
        owner.read_property(P::CameraState).unwrap(),
        V::Integer { value: 0 }
    );
    let image = backend.camera_image(budget.clone()).await.unwrap();
    assert!(std::ptr::eq(
        image.bytes().as_ptr(),
        reader.bytes().as_ptr()
    ));
    assert!(std::ptr::eq(
        owner.wait(id).await.unwrap().bytes().as_ptr(),
        reader.bytes().as_ptr()
    ));
    backend
        .write("gain".into(), Values::from([("Gain".into(), json!(123))]))
        .await
        .unwrap();
    assert_eq!(
        owner.read_property(P::Gain).unwrap(),
        V::Integer { value: 123 }
    );
    assert_eq!(budget.used_bytes(), charged);
    backend.disconnect().await.unwrap();
    drop(image);
    drop(reader);
    idle(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test]
async fn native_capture_owner_controls_cooling_and_explicit_abort_through_shared_actor() {
    for direct in [false, true] {
        let (owner, budget, activity, _) = fixture(direct, json!({"instant":false}));
        let source = source(owner.clone(), &[]);
        let supervisor = supervisor(source.clone(), budget.clone(), activity.clone());
        let first = supervisor.connect().await.unwrap();
        let sibling = supervisor.connect().await.unwrap();
        first.set(S::NumX(64)).await.unwrap();
        first.set(S::NumY(64)).await.unwrap();
        let generation = source.snapshot().generation;
        first
            .start(ExposureRequest {
                duration_seconds: 3.0,
                light: false,
            })
            .await
            .unwrap();
        assert_eq!(
            sibling.set(S::CoolerOn(false)).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(
            sibling.set(S::Gain(123)).await.unwrap_err().kind,
            ErrorKind::Busy
        );
        assert_eq!(first.stop().await.unwrap_err().kind, ErrorKind::Unsupported);
        first.set(S::CoolerOn(false)).await.unwrap();
        first.set(S::SetCcdTemperature(-20.0)).await.unwrap();
        assert_eq!(
            sibling.property(P::CoolerOn).await.unwrap(),
            V::Boolean { value: false }
        );
        assert_eq!(
            sibling.property(P::SetCcdTemperature).await.unwrap(),
            V::Number { value: -20.0 }
        );
        first.abort().await.unwrap();
        until(|| activity.active() == 0).await;
        assert!(!supervisor.status().image_ready);
        assert_eq!(source.snapshot().generation, generation);
        assert!(!source.snapshot().write_uncertain);
        assert!(sibling.connected());
        assert_eq!(budget.used_bytes(), 0);
        // The retired core handle is restored by this explicit setting, without
        // replacing the logical source session or replaying the aborted capture.
        first.set(S::Gain(123)).await.unwrap();
        assert_eq!(
            sibling.property(P::Gain).await.unwrap(),
            V::Integer { value: 123 }
        );
        assert_eq!(source.snapshot().generation, generation);
        first
            .start(ExposureRequest {
                duration_seconds: 0.01,
                light: false,
            })
            .await
            .unwrap();
        until(|| supervisor.status().image_ready).await;
        assert_eq!(source.snapshot().generation, generation);
        drop(first);
        drop(sibling);
        drop(supervisor);
        source.shutdown().await.unwrap();
        idle(&owner, &activity).await;
    }
}

#[test]
fn native_sample_plans_validate_before_copying_or_any_worker_io() {
    let (owner, budget, activity, events) = fixture(false, json!({"instant":true}));
    let sample = P::Gain.sample_request();
    let mut parameter = sample.clone();
    parameter.parameters.insert("extra".into(), json!(1));
    let mut age = sample.clone();
    age.sensor_age = Some("gain".into());
    let mut unknown = sample.clone();
    unknown.member = "unknown".into();
    for (samples, kind) in [
        (vec![sample.clone(); 1025], ErrorKind::InvalidValue),
        (vec![sample.clone(), sample], ErrorKind::InvalidValue),
        (vec![parameter], ErrorKind::InvalidValue),
        (vec![age], ErrorKind::InvalidValue),
        (vec![unknown], ErrorKind::Unsupported),
    ] {
        assert_eq!(
            NativeCameraBackend::new(owner.clone(), samples)
                .err()
                .unwrap()
                .kind,
            kind
        );
    }
    assert!(!owner.snapshot().connected);
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(activity.active(), 0);
}

#[tokio::test]
async fn core_capture_recovery_keeps_source_generation_and_cached_reads_until_explicit_abort() {
    let (owner, budget, activity, events) = fixture_recovery(
        false,
        json!({"instant":true,"failedStatuses":1}),
        RecoveryOptions {
            max_retries: 1,
            reconnect_delay_seconds: 2.0,
            cooling_sample_seconds: 0.01,
            cooling_stable_samples: 1,
            ..RecoveryOptions::default()
        },
    );
    let source = source(owner.clone(), &[]);
    let supervisor = supervisor(source.clone(), budget.clone(), activity.clone());
    let session = supervisor.connect().await.unwrap();
    session.set(S::NumX(64)).await.unwrap();
    session.set(S::NumY(64)).await.unwrap();
    let generation = source.snapshot().generation;
    session
        .start(ExposureRequest {
            duration_seconds: 0.01,
            light: false,
        })
        .await
        .unwrap();
    until(|| owner.snapshot().core.phase == "Reconnect delay").await;
    assert!(!owner.snapshot().core.control_connection_available);
    for _ in 0..3 {
        assert_eq!(
            session.property(P::CameraState).await.unwrap(),
            V::Integer { value: 1 }
        );
        assert!(matches!(
            session.property(P::CcdTemperature).await.unwrap(),
            V::Number { .. }
        ));
        assert_eq!(source.snapshot().generation, generation);
        assert!(source.snapshot().transport_connected);
    }
    assert_eq!(
        owner.snapshot().operation.unwrap().kind,
        NativeOperationKind::Capturing
    );
    session.abort().await.unwrap();
    drop(session);
    drop(supervisor);
    source.shutdown().await.unwrap();
    idle(&owner, &activity).await;
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.starts_with("capture.retry:"))
            .count(),
        1
    );
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|line| line.contains("session.phase: Starting exposure"))
            .count(),
        1
    );
}

#[tokio::test]
async fn reset_during_control_restoration_fences_reconnect_and_never_dispatches_new_setting() {
    // A hardware failure, rather than a clean abort, requires restoration.
    let (owner, _, activity, _) = fixture(false, json!({"instant":false}));
    let mut backend = NativeCameraBackend::new(owner.clone(), vec![]).unwrap();
    backend.connect().await.unwrap();
    owner.configure_geometry(S::NumX(64)).unwrap();
    owner.configure_geometry(S::NumY(64)).unwrap();
    owner.start_configured(3_000_000, true).unwrap();
    until(|| owner.snapshot().core.phase == "Exposing").await;
    assert!(owner.simulated());
    let pid = owner.snapshot().core.process_id.unwrap().to_string();
    #[cfg(windows)]
    let killed = std::process::Command::new("taskkill")
        .args(["/PID", &pid, "/F"])
        .output()
        .unwrap();
    #[cfg(not(windows))]
    let killed = std::process::Command::new("kill")
        .args(["-KILL", &pid])
        .output()
        .unwrap();
    assert!(killed.status.success());
    backend
        .write("abortexposure".into(), Values::new())
        .await
        .unwrap();
    assert!(!owner.snapshot().core.control_connection_available);
    assert_eq!(
        backend
            .write(
                "gain".into(),
                Values::from([("Gain".into(), json!(i32::MAX))])
            )
            .await
            .unwrap_err()
            .kind,
        ErrorKind::InvalidValue
    );
    assert!(owner.snapshot().operation.is_none());
    let mut setting =
        Box::pin(backend.write("gain".into(), Values::from([("Gain".into(), json!(123))])));
    assert!(futures_util::poll!(setting.as_mut()).is_pending());
    assert_eq!(
        owner.snapshot().operation.unwrap().kind,
        NativeOperationKind::Configuring
    );
    drop(setting);
    backend.reset();
    idle(&owner, &activity).await;
    assert_eq!(
        backend.connect_step().await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    backend.disconnect().await.unwrap();
    backend.connect().await.unwrap();
    assert_eq!(
        owner.read_property(P::Gain).unwrap(),
        V::Integer { value: 100 }
    );
    backend.disconnect().await.unwrap();
    idle(&owner, &activity).await;
}
