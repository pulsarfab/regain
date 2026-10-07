//! Private source fixtures, virtual clocks and explicit faults; no equipment I/O.
use regain_hub::{
    activity::ActivityCounter,
    camera::{
        acquisition::{
            AcquisitionPhase as Phase, AcquisitionTiming, CameraSession, CameraSupervisor,
            ExposureRequest,
        },
        image::{CameraImage, ElementType, ImageBudget, ImageDescriptor, ImageOrder},
    },
    parameters::PollPolicy,
    safety::MonotonicClock,
    source::{Backend, BackendFuture, ErrorKind, SourceError, SourceHandle, Values},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};
use tokio::sync::Notify;
use uuid::Uuid;

struct Device {
    values: Mutex<Values>,
    errors: Mutex<BTreeMap<String, SourceError>>,
    starts: AtomicUsize,
    stops: AtomicUsize,
    aborts: AtomicUsize,
    downloads: AtomicUsize,
    disconnects: AtomicUsize,
    resets: AtomicUsize,
    uncertain_start: AtomicBool,
    uncertain_abort: AtomicBool,
    hold_start: AtomicBool,
    hold_prepare: AtomicBool,
    hold_capability: AtomicBool,
    hold_abort: AtomicBool,
    prepare_reads: AtomicUsize,
    capability_reads: AtomicUsize,
    hold_download: AtomicBool,
    replace_download: AtomicBool,
    wrong_shape: AtomicBool,
    release_start: Notify,
    release_prepare: Notify,
    release_capability: Notify,
    release_abort: Notify,
    release_download: Notify,
}
impl Default for Device {
    fn default() -> Self {
        Self {
            values: Mutex::new(Values::from([
                ("camerastate".into(), json!(0)),
                ("imageready".into(), json!(false)),
                ("exposuremin".into(), json!(0.1)),
                ("exposuremax".into(), json!(10.0)),
                ("numx".into(), json!(3)),
                ("numy".into(), json!(2)),
                ("binx".into(), json!(1)),
                ("biny".into(), json!(1)),
                ("startx".into(), json!(0)),
                ("starty".into(), json!(0)),
                ("cameraxsize".into(), json!(12)),
                ("cameraysize".into(), json!(8)),
                ("maxbinx".into(), json!(4)),
                ("maxbiny".into(), json!(4)),
                ("canasymmetricbin".into(), json!(false)),
                ("canabortexposure".into(), json!(true)),
                ("canstopexposure".into(), json!(true)),
                ("lastexposureduration".into(), json!(1.125)),
                (
                    "lastexposurestarttime".into(),
                    json!("2026-10-07T00:00:01.123"),
                ),
            ])),
            errors: Mutex::new(BTreeMap::new()),
            starts: AtomicUsize::new(0),
            stops: AtomicUsize::new(0),
            aborts: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
            resets: AtomicUsize::new(0),
            uncertain_start: AtomicBool::new(false),
            uncertain_abort: AtomicBool::new(false),
            hold_start: AtomicBool::new(false),
            hold_prepare: AtomicBool::new(false),
            hold_capability: AtomicBool::new(false),
            hold_abort: AtomicBool::new(false),
            prepare_reads: AtomicUsize::new(0),
            capability_reads: AtomicUsize::new(0),
            hold_download: AtomicBool::new(false),
            replace_download: AtomicBool::new(false),
            wrong_shape: AtomicBool::new(false),
            release_start: Notify::new(),
            release_prepare: Notify::new(),
            release_capability: Notify::new(),
            release_abort: Notify::new(),
            release_download: Notify::new(),
        }
    }
}
impl Device {
    fn set(&self, key: &str, value: Value) {
        self.values.lock().unwrap().insert(key.into(), value);
    }
    fn complete(&self) {
        self.set("imageready", json!(true));
        self.set("camerastate", json!(0));
    }
}
struct Mock(Arc<Device>);
impl Backend for Mock {
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        self.0.disconnects.fetch_add(1, SeqCst);
        Box::pin(async { Ok(()) })
    }
    fn reset(&mut self) {
        self.0.resets.fetch_add(1, SeqCst);
    }
    fn read(&mut self, member: String, _: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            if member == "exposuremin" && self.0.hold_prepare.load(SeqCst) {
                self.0.prepare_reads.fetch_add(1, SeqCst);
                self.0.release_prepare.notified().await;
            }
            if member == "canabortexposure" && self.0.hold_capability.load(SeqCst) {
                self.0.capability_reads.fetch_add(1, SeqCst);
                self.0.release_capability.notified().await;
            }
            if let Some(error) = self.0.errors.lock().unwrap().get(&member) {
                return Err(error.clone());
            }
            self.0
                .values
                .lock()
                .unwrap()
                .get(&member)
                .cloned()
                .ok_or_else(|| {
                    SourceError::new(ErrorKind::Unsupported, "Private unsupported property")
                })
        })
    }
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        Box::pin(async move {
            match member.as_str() {
                "startexposure" => {
                    assert!(parameters["Duration"].is_number());
                    assert!(parameters["Light"].is_boolean());
                    self.0.starts.fetch_add(1, SeqCst);
                    self.0.set("imageready", json!(false));
                    self.0.set("camerastate", json!(2));
                    if self.0.hold_start.load(SeqCst) {
                        self.0.release_start.notified().await;
                    }
                    if self.0.uncertain_start.load(SeqCst) {
                        return Err(SourceError::uncertain());
                    }
                }
                "stopexposure" => {
                    assert!(parameters.is_empty());
                    self.0.stops.fetch_add(1, SeqCst);
                    self.0.set("lastexposureduration", json!(0.25));
                    self.0.complete();
                }
                "abortexposure" => {
                    assert!(parameters.is_empty());
                    self.0.aborts.fetch_add(1, SeqCst);
                    self.0.set("imageready", json!(false));
                    self.0.set("camerastate", json!(0));
                    if self.0.hold_abort.load(SeqCst) {
                        self.0.release_abort.notified().await;
                    }
                    if self.0.uncertain_abort.load(SeqCst) {
                        return Err(SourceError::uncertain());
                    }
                }
                _ => {
                    return Err(SourceError::new(
                        ErrorKind::Unsupported,
                        "Private unsupported command",
                    ));
                }
            }
            Ok(Value::Null)
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async { Ok(self.0.values.lock().unwrap().clone()) })
    }
    fn camera_image(&mut self, budget: ImageBudget) -> BackendFuture<'_, CameraImage> {
        Box::pin(async move {
            self.0.downloads.fetch_add(1, SeqCst);
            let d = ImageDescriptor::new(
                if self.0.wrong_shape.load(SeqCst) {
                    2
                } else {
                    3
                },
                2,
                None,
                ElementType::Int32,
                ElementType::UInt16,
                ImageOrder::Ascom,
            )?;
            let mut allocation = budget.allocate(d)?;
            allocation.bytes_mut().fill(17);
            if self.0.hold_download.load(SeqCst) {
                self.0.release_download.notified().await;
            }
            if self.0.replace_download.load(SeqCst) {
                self.0
                    .set("lastexposurestarttime", json!("2026-10-07T00:00:02.123"));
            }
            Ok(allocation.finish())
        })
    }
}
struct Fixture {
    device: Arc<Device>,
    source: Arc<SourceHandle>,
    supervisor: Arc<CameraSupervisor>,
    budget: ImageBudget,
    activity: ActivityCounter,
}
impl Fixture {
    fn new(memory: usize) -> Self {
        Self::with_poll_interval(memory, Duration::from_millis(10))
    }
    fn with_poll_interval(memory: usize, poll_interval: Duration) -> Self {
        let device = Arc::new(Device::default());
        let source = SourceHandle::spawn(
            Uuid::new_v4(),
            Uuid::new_v4(),
            PollPolicy::default(),
            Box::new(Mock(device.clone())),
            Arc::new(MonotonicClock::default()),
        )
        .unwrap();
        let budget = ImageBudget::new(memory).unwrap();
        let activity = ActivityCounter::default();
        let timing = AcquisitionTiming {
            connection_timeout: Duration::from_secs(2),
            admission_timeout: Duration::from_secs(4),
            readiness_grace: Duration::from_secs(2),
            download_timeout: Duration::from_secs(1),
            poll_interval,
        };
        let supervisor =
            CameraSupervisor::new(source.clone(), budget.clone(), timing, activity.clone())
                .unwrap();
        Self {
            device,
            source,
            supervisor,
            budget,
            activity,
        }
    }
    async fn session(&self) -> CameraSession {
        self.supervisor.connect().await.unwrap()
    }
    async fn ready(&self) {
        for _ in 0..100 {
            settle().await;
            if self.supervisor.status().image_ready {
                return;
            }
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        panic!("Image not published: {:?}", self.supervisor.status());
    }
    async fn phase(&self, expected: Phase) {
        for _ in 0..100 {
            settle().await;
            if self.supervisor.status().phase == expected {
                return;
            }
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        panic!("Expected {expected:?}, got {:?}", self.supervisor.status());
    }
}
async fn settle() {
    for _ in 0..40 {
        tokio::task::yield_now().await;
    }
}
fn request() -> ExposureRequest {
    ExposureRequest {
        duration_seconds: 1.0,
        light: true,
    }
}

#[tokio::test(start_paused = true)]
async fn one_acquisition_survives_owner_loss_and_observers_share_immutable_image() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    let observer = f.session().await;
    let id = owner.start(request()).await.unwrap();
    assert_eq!(f.activity.active(), 1);
    assert_eq!(
        observer.start(request()).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(observer.stop().await.unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(observer.abort().await.unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(
        observer.abandon_uncertain().unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        f.supervisor.abandon_uncertain(id).unwrap_err().kind,
        ErrorKind::Busy
    );
    drop(owner);
    settle().await;
    assert_eq!(f.activity.active(), 1);
    assert_eq!(f.device.disconnects.load(SeqCst), 0);
    f.device.complete();
    f.ready().await;
    let a = observer.image().unwrap();
    let b = observer.image().unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(a.image.bytes(), [17; 12]);
    assert_eq!(a.identity.acquisition, id);
    assert_eq!(a.identity.geometry.width, 3);
    assert_eq!(a.identity.exposure.duration_seconds, Some(1.125));
    assert_eq!(f.activity.active(), 0);
    assert_eq!(f.device.downloads.load(SeqCst), 1);
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    f.device.set("numx", json!(4));
    assert_eq!(b.identity.geometry.width, 3);
    drop(observer);
    settle().await;
    f.source.shutdown().await.unwrap();
    assert_eq!(a.image.bytes(), [17; 12]);
}

#[tokio::test(start_paused = true)]
async fn invalid_start_preserves_completed_image_and_makes_no_equipment_command() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    owner.start(request()).await.unwrap();
    f.device.complete();
    f.ready().await;
    let image = owner.image().unwrap();
    for duration in [f64::NAN, f64::INFINITY, -1.0, 0.0, 11.0] {
        assert_eq!(
            owner
                .start(ExposureRequest {
                    duration_seconds: duration,
                    light: true
                })
                .await
                .unwrap_err()
                .kind,
            ErrorKind::InvalidValue
        );
        assert!(Arc::ptr_eq(&image, &owner.image().unwrap()));
        assert_eq!(f.activity.active(), 0);
    }
    for (member, value) in [
        ("numx", json!(0)),
        ("startx", json!(12)),
        ("binx", json!(2)),
        ("numy", json!(i32::MAX)),
    ] {
        let prior = f.device.values.lock().unwrap()[member].clone();
        f.device.set(member, value);
        assert_eq!(
            owner.start(request()).await.unwrap_err().kind,
            ErrorKind::InvalidValue
        );
        assert!(Arc::ptr_eq(&image, &owner.image().unwrap()));
        f.device.set(member, prior);
    }
    f.device.set("camerastate", json!(2));
    assert_eq!(
        owner.start(request()).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    assert_eq!(f.activity.active(), 0);
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn stop_preserves_shortened_image_abort_discards_and_idle_commands_are_inert() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    owner.stop().await.unwrap();
    owner.abort().await.unwrap();
    assert_eq!(f.device.stops.load(SeqCst), 0);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    owner.start(request()).await.unwrap();
    owner.stop().await.unwrap();
    f.ready().await;
    let first = owner.image().unwrap();
    assert_eq!(first.identity.exposure.duration_seconds, Some(0.25));
    assert_eq!(f.device.stops.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    owner.start(request()).await.unwrap();
    assert!(owner.image().is_err());
    owner.abort().await.unwrap();
    assert_eq!(f.supervisor.status().phase, Phase::Idle);
    assert!(owner.image().is_err());
    assert_eq!(first.image.bytes(), [17; 12]);
    // Acknowledged abort releases control before another immediate admission.
    owner.start(request()).await.unwrap();
    owner.abort().await.unwrap();
    assert_eq!(f.device.starts.load(SeqCst), 3);
    assert_eq!(f.device.aborts.load(SeqCst), 2);
    assert_eq!(f.activity.active(), 0);
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn optional_exposure_metadata_remains_unsupported_without_fabricated_values() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    for member in ["lastexposureduration", "lastexposurestarttime"] {
        f.device.errors.lock().unwrap().insert(
            member.into(),
            SourceError {
                upstream_code: Some(0x400),
                ..SourceError::new(ErrorKind::Unsupported, "Private optional metadata")
            },
        );
    }
    owner
        .start(ExposureRequest {
            duration_seconds: 0.0,
            light: false,
        })
        .await
        .unwrap();
    f.device.complete();
    f.ready().await;
    let image = owner.image().unwrap();
    assert_eq!(image.identity.request.duration_seconds, 0.0);
    assert_eq!(image.identity.exposure.duration_seconds, None);
    assert_eq!(image.identity.exposure.start_time, None);
    assert_eq!(
        image
            .identity
            .exposure
            .duration_error
            .as_ref()
            .unwrap()
            .upstream_code,
        Some(0x400)
    );
    assert_eq!(
        image
            .identity
            .exposure
            .start_time_error
            .as_ref()
            .unwrap()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(f.activity.active(), 0);
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn applied_uncertain_start_keeps_control_activity_and_fence_until_explicit_abandonment() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    f.device.uncertain_start.store(true, SeqCst);
    assert_eq!(
        owner.start(request()).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(f.supervisor.status().phase, Phase::Uncertain);
    assert_eq!(f.activity.active(), 1);
    assert!(f.source.snapshot().write_uncertain);
    let id = f.supervisor.status().acquisition.unwrap();
    assert_eq!(
        f.supervisor
            .abandon_uncertain(Uuid::new_v4())
            .unwrap_err()
            .kind,
        ErrorKind::Unavailable
    );
    drop(owner);
    settle().await;
    assert_eq!(f.activity.active(), 1);
    assert_eq!(f.device.disconnects.load(SeqCst), 0);
    // The retained uncertain lease recovers on the configured 30-second source
    // polling cycle. A new client must not bypass that policy to clear a fence.
    tokio::time::advance(Duration::from_secs(30)).await;
    settle().await;
    assert!(f.source.snapshot().transport_connected);
    let observer = f.session().await;
    assert_eq!(
        observer.start(request()).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(
        observer.abandon_uncertain().unwrap_err().kind,
        ErrorKind::Busy
    );
    f.supervisor.abandon_uncertain(id).unwrap();
    settle().await;
    assert_eq!(f.activity.active(), 0);
    assert!(f.source.snapshot().write_uncertain);
    assert_eq!(
        observer.start(request()).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    drop(observer);
    settle().await;
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_start_waiter_after_dispatch_does_not_cancel_owned_capture() {
    let f = Fixture::new(24);
    let owner = Arc::new(f.session().await);
    f.device.hold_start.store(true, SeqCst);
    let task = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.start(request()).await })
    };
    for _ in 0..100 {
        settle().await;
        if f.device.starts.load(SeqCst) == 1 {
            break;
        }
    }
    assert_eq!(f.device.starts.load(SeqCst), 1);
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    drop(owner);
    f.device.release_start.notify_one();
    settle().await;
    let observer = f.session().await;
    assert_eq!(f.activity.active(), 1);
    f.device.complete();
    f.ready().await;
    assert_eq!(observer.image().unwrap().image.bytes(), [17; 12]);
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    assert_eq!(f.activity.active(), 0);
    drop(observer);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn downloading_rejects_abort_ignores_stop_and_does_not_actuate_on_disconnect() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    let observer = f.session().await;
    f.device.hold_download.store(true, SeqCst);
    owner.start(request()).await.unwrap();
    f.device.complete();
    f.phase(Phase::Downloading).await;
    for _ in 0..100 {
        settle().await;
        if f.device.downloads.load(SeqCst) == 1 {
            break;
        }
    }
    assert_eq!(f.device.downloads.load(SeqCst), 1);
    assert_eq!(f.budget.used_bytes(), 12);
    assert_eq!(owner.abort().await.unwrap_err().kind, ErrorKind::Busy);
    owner.stop().await.unwrap();
    drop(owner);
    settle().await;
    assert_eq!(f.activity.active(), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    assert_eq!(f.device.stops.load(SeqCst), 0);
    f.device.release_download.notify_one();
    f.ready().await;
    assert_eq!(observer.image().unwrap().image.bytes(), [17; 12]);
    assert_eq!(f.activity.active(), 0);
    drop(observer);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn malformed_capabilities_and_unsupported_stop_never_become_actuator_commands() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    owner.start(request()).await.unwrap();
    f.device.set("canstopexposure", json!(false));
    assert_eq!(owner.stop().await.unwrap_err().kind, ErrorKind::Unsupported);
    f.device.set("canabortexposure", json!("true"));
    assert_eq!(
        owner.abort().await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(f.device.stops.load(SeqCst), 0);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    f.device.set("canabortexposure", json!(true));
    owner.abort().await.unwrap();
    f.device.set("camerastate", json!("0"));
    assert_eq!(
        owner.start(request()).await.unwrap_err().kind,
        ErrorKind::Unavailable
    );
    assert_eq!(f.activity.active(), 0);
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn readiness_and_download_failures_retain_ownership_without_exposure_replay() {
    for download in [false, true] {
        let f = Fixture::new(24);
        let owner = f.session().await;
        f.device.hold_download.store(download, SeqCst);
        owner.start(request()).await.unwrap();
        if download {
            f.device.complete();
            f.phase(Phase::Downloading).await;
        }
        tokio::time::advance(Duration::from_secs(4)).await;
        settle().await;
        assert_eq!(f.supervisor.status().phase, Phase::Uncertain);
        assert_eq!(f.activity.active(), 1);
        assert!(owner.image().is_err());
        assert_eq!(f.budget.used_bytes(), 0);
        assert_eq!(f.device.starts.load(SeqCst), 1);
        assert_eq!(f.device.aborts.load(SeqCst), 0);
        owner.abandon_uncertain().unwrap();
        assert_eq!(f.activity.active(), 0);
        drop(owner);
        f.source.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn image_geometry_or_identity_replacement_is_not_published() {
    for wrong_shape in [false, true] {
        let f = Fixture::new(24);
        let owner = f.session().await;
        f.device.wrong_shape.store(wrong_shape, SeqCst);
        f.device.replace_download.store(!wrong_shape, SeqCst);
        owner.start(request()).await.unwrap();
        f.device.complete();
        f.phase(Phase::Uncertain).await;
        assert!(owner.image().is_err());
        assert_eq!(f.budget.used_bytes(), 0);
        assert_eq!(f.activity.active(), 1);
        assert_eq!(f.device.starts.load(SeqCst), 1);
        assert_eq!(f.device.downloads.load(SeqCst), 1);
        owner.abandon_uncertain().unwrap();
        drop(owner);
        f.source.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn pinned_old_readers_bound_later_captures_without_eviction_or_reexposure() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    owner.start(request()).await.unwrap();
    f.device.complete();
    f.ready().await;
    let first = owner.image().unwrap();
    owner.start(request()).await.unwrap();
    f.device.complete();
    f.ready().await;
    let second = owner.image().unwrap();
    assert_eq!(f.budget.used_bytes(), 24);
    assert!(!Arc::ptr_eq(&first, &second));
    owner.start(request()).await.unwrap();
    f.device.complete();
    f.phase(Phase::Uncertain).await;
    assert_eq!(f.supervisor.status().error.unwrap().kind, ErrorKind::Busy);
    assert_eq!(first.image.bytes(), [17; 12]);
    assert_eq!(second.image.bytes(), [17; 12]);
    assert_eq!(f.device.starts.load(SeqCst), 3);
    assert_eq!(f.device.downloads.load(SeqCst), 3);
    owner.abandon_uncertain().unwrap();
    drop(first);
    drop(second);
    assert_eq!(f.budget.used_bytes(), 0);
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_start_during_preflight_never_sends_an_exposure() {
    let f = Fixture::new(24);
    let owner = Arc::new(f.session().await);
    f.device.hold_prepare.store(true, SeqCst);
    let task = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.start(request()).await })
    };
    settle().await;
    assert_eq!(f.device.prepare_reads.load(SeqCst), 1);
    assert_eq!(f.activity.active(), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    f.device.release_prepare.notify_one();
    settle().await;
    assert_eq!(f.supervisor.status().phase, Phase::Idle);
    assert_eq!(f.activity.active(), 0);
    assert_eq!(f.device.starts.load(SeqCst), 0);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    // Preflight cancellation must release control for a later real request.
    f.device.hold_prepare.store(false, SeqCst);
    owner.start(request()).await.unwrap();
    owner.abort().await.unwrap();
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn cancelled_abort_during_capability_read_restores_the_existing_capture() {
    let f = Fixture::new(24);
    let owner = Arc::new(f.session().await);
    owner.start(request()).await.unwrap();
    f.device.hold_capability.store(true, SeqCst);
    let task = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.abort().await })
    };
    settle().await;
    assert_eq!(f.device.capability_reads.load(SeqCst), 1);
    assert_eq!(f.supervisor.status().phase, Phase::Aborting);
    assert_eq!(owner.stop().await.unwrap_err().kind, ErrorKind::Busy);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    f.device.release_capability.notify_one();
    settle().await;
    assert_eq!(f.supervisor.status().phase, Phase::Exposing);
    assert_eq!(f.activity.active(), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    f.device.complete();
    f.ready().await;
    assert_eq!(owner.image().unwrap().image.bytes(), [17; 12]);
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.downloads.load(SeqCst), 1);
    drop(owner);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn dispatched_abort_finishes_after_waiter_cancellation_and_wakes_long_poll() {
    let f = Fixture::with_poll_interval(24, Duration::from_secs(60));
    let owner = Arc::new(f.session().await);
    let observer = f.session().await;
    owner.start(request()).await.unwrap();
    settle().await;
    f.device.hold_abort.store(true, SeqCst);
    let task = {
        let owner = owner.clone();
        tokio::spawn(async move { owner.abort().await })
    };
    settle().await;
    assert_eq!(f.device.aborts.load(SeqCst), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(owner);
    f.device.release_abort.notify_one();
    settle().await;
    assert_eq!(f.supervisor.status().phase, Phase::Idle);
    assert_eq!(f.activity.active(), 0);
    assert!(!f.supervisor.status().image_ready);
    assert_eq!(f.device.downloads.load(SeqCst), 0);
    // No virtual time advances: the previous monitor must drop its lease now,
    // rather than keeping exclusive control until its 60-second poll timer.
    observer.start(request()).await.unwrap();
    f.device.hold_abort.store(false, SeqCst);
    observer.abort().await.unwrap();
    assert_eq!(f.device.aborts.load(SeqCst), 2);
    assert_eq!(f.device.starts.load(SeqCst), 2);
    drop(observer);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn uncertain_abort_preserves_activity_and_source_fence_without_replay() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    owner.start(request()).await.unwrap();
    f.device.uncertain_abort.store(true, SeqCst);
    assert_eq!(owner.abort().await.unwrap_err().kind, ErrorKind::Uncertain);
    assert_eq!(f.supervisor.status().phase, Phase::Uncertain);
    assert_eq!(f.activity.active(), 1);
    assert!(f.source.snapshot().write_uncertain);
    let id = f.supervisor.status().acquisition.unwrap();
    drop(owner);
    settle().await;
    tokio::time::advance(Duration::from_secs(30)).await;
    settle().await;
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 1);
    assert_eq!(f.device.stops.load(SeqCst), 0);
    assert_eq!(f.device.downloads.load(SeqCst), 0);
    assert_eq!(f.activity.active(), 1);
    // Keep an independent client connected. With no clients at all the shared
    // source deliberately tears down its last lease and starts a fresh epoch.
    let observer = f.session().await;
    f.supervisor.abandon_uncertain(id).unwrap();
    settle().await;
    assert_eq!(f.activity.active(), 0);
    assert!(f.source.snapshot().write_uncertain);
    assert_eq!(
        observer.start(request()).await.unwrap_err().kind,
        ErrorKind::Uncertain
    );
    drop(observer);
    f.source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn optional_metadata_members_are_independent() {
    for missing in ["lastexposureduration", "lastexposurestarttime"] {
        let f = Fixture::new(24);
        let owner = f.session().await;
        f.device.errors.lock().unwrap().insert(
            missing.into(),
            SourceError::new(ErrorKind::Unsupported, "Private optional member"),
        );
        owner.start(request()).await.unwrap();
        f.device.complete();
        f.ready().await;
        let image = owner.image().unwrap();
        let metadata = &image.identity.exposure;
        assert_eq!(
            metadata.duration_seconds.is_none(),
            missing == "lastexposureduration"
        );
        assert_eq!(
            metadata.duration_error.is_some(),
            missing == "lastexposureduration"
        );
        assert_eq!(
            metadata.start_time.is_none(),
            missing == "lastexposurestarttime"
        );
        assert_eq!(
            metadata.start_time_error.is_some(),
            missing == "lastexposurestarttime"
        );
        assert_eq!(f.device.downloads.load(SeqCst), 1);
        drop(image);
        drop(owner);
        f.source.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn utc_start_times_are_preserved_and_malformed_metadata_never_publishes() {
    for start in [
        "2024-02-29T23:59:59",
        "2000-02-29T12:00:00.123456789Z",
        "2026-10-07T00:00:01+00:00",
        "2026-10-07T00:00:01.1",
    ] {
        let f = Fixture::new(24);
        let owner = f.session().await;
        f.device.set("lastexposurestarttime", json!(start));
        owner.start(request()).await.unwrap();
        f.device.complete();
        f.ready().await;
        assert_eq!(
            owner
                .image()
                .unwrap()
                .identity
                .exposure
                .start_time
                .as_deref(),
            Some(start)
        );
        drop(owner);
        f.source.shutdown().await.unwrap();
    }
    for (member, value) in [
        ("lastexposurestarttime", json!("1900-02-29T00:00:00")),
        ("lastexposurestarttime", json!("2026-02-29T00:00:00")),
        ("lastexposurestarttime", json!("2026-04-31T00:00:00")),
        ("lastexposurestarttime", json!("2026-00-01T00:00:00")),
        ("lastexposurestarttime", json!("2026-10-07T24:00:00")),
        ("lastexposurestarttime", json!("2026-10-07T00:60:00")),
        ("lastexposurestarttime", json!("2026-10-07T00:00:61")),
        ("lastexposurestarttime", json!("2026-10-07T00:00:00.")),
        ("lastexposurestarttime", json!("2026-10-07T00:00:00+01:00")),
        ("lastexposurestarttime", json!("2026-10-07T00:00:00.é")),
        ("lastexposurestarttime", json!(123)),
        ("lastexposureduration", json!(-0.5)),
        ("lastexposureduration", json!("1.0")),
    ] {
        let f = Fixture::new(24);
        let owner = f.session().await;
        f.device.set(member, value);
        owner.start(request()).await.unwrap();
        f.device.complete();
        f.phase(Phase::Uncertain).await;
        assert_eq!(
            f.supervisor.status().error.unwrap().kind,
            ErrorKind::Unavailable
        );
        assert!(!f.supervisor.status().image_ready);
        assert_eq!(f.device.downloads.load(SeqCst), 0);
        assert_eq!(f.activity.active(), 1);
        owner.abandon_uncertain().unwrap();
        drop(owner);
        f.source.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn malformed_readiness_preserves_uncertain_ownership_without_download() {
    for (member, value) in [
        ("imageready", json!("true")),
        ("camerastate", json!(6)),
        ("camerastate", json!(5)),
        ("camerastate", json!("0")),
    ] {
        let f = Fixture::new(24);
        let owner = f.session().await;
        owner.start(request()).await.unwrap();
        f.device.set(member, value);
        f.phase(Phase::Uncertain).await;
        assert_eq!(
            f.supervisor.status().error.unwrap().kind,
            ErrorKind::Unavailable
        );
        assert_eq!(f.activity.active(), 1);
        assert_eq!(f.device.downloads.load(SeqCst), 0);
        assert_eq!(f.device.aborts.load(SeqCst), 0);
        owner.abandon_uncertain().unwrap();
        drop(owner);
        f.source.shutdown().await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn generation_loss_during_download_never_publishes_the_old_frame() {
    let f = Fixture::new(24);
    let owner = f.session().await;
    let generation = f.source.snapshot().generation;
    owner.start(request()).await.unwrap();
    f.device.hold_download.store(true, SeqCst);
    f.device.complete();
    f.phase(Phase::Downloading).await;
    settle().await;
    assert_eq!(f.budget.used_bytes(), 12);
    // The byte copy finishes, but the following identity read loses its session.
    // Even valid pixels must not become a completed image of a retired epoch.
    f.device.errors.lock().unwrap().insert(
        "lastexposurestarttime".into(),
        SourceError {
            transport_lost: true,
            ..SourceError::new(ErrorKind::Disconnected, "Private session lost")
        },
    );
    f.device.release_download.notify_one();
    f.phase(Phase::Uncertain).await;
    assert_ne!(f.source.snapshot().generation, generation);
    assert!(!owner.connected());
    assert!(owner.image().is_err());
    assert!(!f.supervisor.status().image_ready);
    assert_eq!(f.budget.used_bytes(), 0);
    assert_eq!(f.activity.active(), 1);
    assert_eq!(f.device.starts.load(SeqCst), 1);
    assert_eq!(f.device.downloads.load(SeqCst), 1);
    assert_eq!(f.device.aborts.load(SeqCst), 0);
    owner.abandon_uncertain().unwrap();
    drop(owner);
    f.source.shutdown().await.unwrap();
}
