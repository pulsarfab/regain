//! Binary source dispatch uses private actors; no physical transport is opened.
use regain_hub::{
    camera::image::{CameraImage, ElementType, ImageBudget, ImageDescriptor, ImageOrder},
    parameters::PollPolicy,
    safety::MonotonicClock,
    source::{Backend, BackendFuture, ErrorKind, SourceError, SourceHandle, Values},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    },
    time::Duration,
};
use uuid::Uuid;

#[derive(Default)]
struct Device {
    downloads: AtomicUsize,
    disconnects: AtomicUsize,
    writes: AtomicUsize,
    resets: AtomicUsize,
    hang_image: AtomicBool,
    uncertain_write: AtomicBool,
    image_transport_lost: AtomicBool,
    slow_image: AtomicBool,
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
    fn read(&mut self, _: String, _: Values) -> BackendFuture<'_, Value> {
        Box::pin(async { Ok(json!(true)) })
    }
    fn write(&mut self, _: String, _: Values) -> BackendFuture<'_, Value> {
        self.0.writes.fetch_add(1, SeqCst);
        Box::pin(async {
            if self.0.uncertain_write.load(SeqCst) {
                Err(SourceError::uncertain())
            } else {
                Ok(Value::Null)
            }
        })
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        Box::pin(async { Ok(Values::from([("camerastate".into(), json!(0))])) })
    }
    fn reset(&mut self) {
        self.0.resets.fetch_add(1, SeqCst);
    }
    fn camera_image(&mut self, budget: ImageBudget) -> BackendFuture<'_, CameraImage> {
        Box::pin(async move {
            self.0.downloads.fetch_add(1, SeqCst);
            let descriptor = ImageDescriptor::new(
                3,
                2,
                None,
                ElementType::Int32,
                ElementType::UInt16,
                ImageOrder::Ascom,
            )?;
            let mut allocation = budget.allocate(descriptor)?;
            allocation.bytes_mut().copy_from_slice(&[1; 12]);
            if self.0.hang_image.load(SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.0.slow_image.load(SeqCst) {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            if self.0.image_transport_lost.load(SeqCst) {
                return Err(SourceError {
                    transport_lost: true,
                    ..SourceError::new(ErrorKind::Disconnected, "Private image transport lost")
                });
            }
            Ok(allocation.finish())
        })
    }
}
fn spawn(device: &Arc<Device>) -> Arc<SourceHandle> {
    SourceHandle::spawn(
        Uuid::new_v4(),
        Uuid::new_v4(),
        PollPolicy::default(),
        Box::new(Mock(device.clone())),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap()
}
async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn only_control_owner_can_download_and_readers_outlive_source_disconnect() {
    let device = Arc::new(Device::default());
    let source = spawn(&device);
    let owner = Uuid::new_v4();
    let observer = Uuid::new_v4();
    let budget = ImageBudget::new(12).unwrap();
    let deadline = Duration::from_secs(5);
    assert_eq!(
        source
            .camera_image_fenced(
                owner,
                source.snapshot().generation,
                budget.clone(),
                deadline
            )
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Disconnected
    );
    source.acquire(owner).await.unwrap();
    source.acquire(observer).await.unwrap();
    let generation = source.snapshot().generation;
    assert_eq!(
        source
            .camera_image_fenced(observer, generation, budget.clone(), deadline)
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Busy
    );
    assert_eq!(device.downloads.load(SeqCst), 0);
    source.control(owner, true).await.unwrap();
    assert_eq!(
        source.control(observer, true).await.unwrap_err().kind,
        ErrorKind::Busy
    );
    assert_eq!(
        source
            .camera_image_fenced(owner, Uuid::new_v4(), budget.clone(), deadline)
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(device.downloads.load(SeqCst), 0);
    let image = source
        .camera_image_fenced(owner, generation, budget.clone(), deadline)
        .await
        .unwrap();
    let reader = image.clone();
    assert_eq!(device.downloads.load(SeqCst), 1);
    assert_eq!(source.snapshot().generation, generation);
    assert!(!source.snapshot().values.contains_key("imagearray"));
    source.release(owner).await.unwrap();
    assert_eq!(device.disconnects.load(SeqCst), 0);
    source.release(observer).await.unwrap();
    assert_eq!(device.disconnects.load(SeqCst), 1);
    source.shutdown().await.unwrap();
    drop(image);
    assert_eq!(reader.bytes(), [1; 12]);
    assert_eq!(budget.used_bytes(), 12);
    drop(reader);
    assert_eq!(budget.used_bytes(), 0);
}

#[tokio::test(start_paused = true)]
async fn image_deadline_is_independent_of_scalar_polling_deadline() {
    let device = Arc::new(Device::default());
    device.slow_image.store(true, SeqCst);
    let source = spawn(&device);
    let owner = Uuid::new_v4();
    source.acquire(owner).await.unwrap();
    source.control(owner, true).await.unwrap();
    let generation = source.snapshot().generation;
    let budget = ImageBudget::new(12).unwrap();
    // Scalar timeout is one second; the private image takes two seconds.
    let image = source
        .camera_image_fenced(owner, generation, budget.clone(), Duration::from_secs(3))
        .await
        .unwrap();
    assert_eq!(image.bytes(), [1; 12]);
    assert_eq!(source.snapshot().generation, generation);
    assert_eq!(device.resets.load(SeqCst), 0);
    drop(image);
    for deadline in [Duration::ZERO, Duration::from_secs(3601)] {
        assert_eq!(
            source
                .camera_image_fenced(owner, generation, budget.clone(), deadline)
                .await
                .err()
                .unwrap()
                .kind,
            ErrorKind::InvalidValue
        );
    }
    assert_eq!(device.downloads.load(SeqCst), 1);
    source.release(owner).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn partial_download_timeout_retires_transport_without_replaying_or_fencing_a_write() {
    let device = Arc::new(Device::default());
    device.hang_image.store(true, SeqCst);
    let source = spawn(&device);
    let owner = Uuid::new_v4();
    source.acquire(owner).await.unwrap();
    source.control(owner, true).await.unwrap();
    let generation = source.snapshot().generation;
    let budget = ImageBudget::new(12).unwrap();
    let result = source
        .camera_image_fenced(owner, generation, budget.clone(), Duration::from_secs(1))
        .await;
    assert_eq!(result.err().unwrap().kind, ErrorKind::Transient);
    assert_eq!(budget.used_bytes(), 0);
    assert_ne!(source.snapshot().generation, generation);
    assert!(!source.snapshot().write_uncertain);
    assert_eq!(device.resets.load(SeqCst), 1);
    assert_eq!(device.downloads.load(SeqCst), 1);
    assert_eq!(device.writes.load(SeqCst), 0);
    device.hang_image.store(false, SeqCst);
    assert_eq!(
        source
            .camera_image_fenced(owner, generation, budget.clone(), Duration::from_secs(1))
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Unavailable
    );
    assert_eq!(device.downloads.load(SeqCst), 1);
    source.release(owner).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn abandoned_waiter_keeps_dispatched_download_bounded_and_releases_its_result() {
    let device = Arc::new(Device::default());
    device.hang_image.store(true, SeqCst);
    let source = spawn(&device);
    let owner = Uuid::new_v4();
    source.acquire(owner).await.unwrap();
    source.control(owner, true).await.unwrap();
    let generation = source.snapshot().generation;
    let budget = ImageBudget::new(12).unwrap();
    let task = {
        let source = source.clone();
        let budget = budget.clone();
        tokio::spawn(async move {
            source
                .camera_image_fenced(owner, generation, budget, Duration::from_secs(1))
                .await
        })
    };
    settle().await;
    assert_eq!(device.downloads.load(SeqCst), 1);
    assert_eq!(budget.used_bytes(), 12);
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(device.downloads.load(SeqCst), 1);
    assert_eq!(device.writes.load(SeqCst), 0);
    source.release(owner).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test]
async fn reconciliation_image_read_does_not_clear_an_uncertain_command_fence() {
    let device = Arc::new(Device::default());
    device.uncertain_write.store(true, SeqCst);
    let source = spawn(&device);
    let owner = Uuid::new_v4();
    source.acquire(owner).await.unwrap();
    source.control(owner, true).await.unwrap();
    assert_eq!(
        source
            .write(owner, "startexposure", Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert!(source.snapshot().write_uncertain);
    // A scalar reconciliation read establishes a new transport generation.
    source
        .read(owner, "imageready", Values::new())
        .await
        .unwrap();
    let generation = source.snapshot().generation;
    let image = source
        .camera_image_fenced(
            owner,
            generation,
            ImageBudget::new(12).unwrap(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
    assert_eq!(image.bytes(), [1; 12]);
    assert!(source.snapshot().write_uncertain);
    assert_eq!(
        source
            .write(owner, "startexposure", Values::new())
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Uncertain
    );
    assert_eq!(device.writes.load(SeqCst), 1);
    source.release(owner).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test]
async fn definitive_image_transport_loss_changes_generation_and_returns_original_error() {
    let device = Arc::new(Device::default());
    device.image_transport_lost.store(true, SeqCst);
    let source = spawn(&device);
    let owner = Uuid::new_v4();
    source.acquire(owner).await.unwrap();
    source.control(owner, true).await.unwrap();
    let generation = source.snapshot().generation;
    let budget = ImageBudget::new(12).unwrap();
    let error = source
        .camera_image_fenced(owner, generation, budget.clone(), Duration::from_secs(1))
        .await
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Disconnected);
    assert!(error.transport_lost);
    assert_ne!(source.snapshot().generation, generation);
    assert_eq!(budget.used_bytes(), 0);
    assert_eq!(device.downloads.load(SeqCst), 1);
    assert_eq!(device.resets.load(SeqCst), 1);
    assert!(!source.snapshot().write_uncertain);
    source.release(owner).await.unwrap();
    source.shutdown().await.unwrap();
}

struct ScalarOnly(Mock);
impl Backend for ScalarOnly {
    fn connect(&mut self) -> BackendFuture<'_, ()> {
        self.0.connect()
    }
    fn disconnect(&mut self) -> BackendFuture<'_, ()> {
        self.0.disconnect()
    }
    fn read(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        self.0.read(member, parameters)
    }
    fn write(&mut self, member: String, parameters: Values) -> BackendFuture<'_, Value> {
        self.0.write(member, parameters)
    }
    fn poll(&mut self) -> BackendFuture<'_, Values> {
        self.0.poll()
    }
    fn reset(&mut self) {
        self.0.reset();
    }
}

#[tokio::test]
async fn existing_scalar_adapters_explicitly_reject_binary_images_without_transport_reset() {
    let device = Arc::new(Device::default());
    let source = SourceHandle::spawn(
        Uuid::new_v4(),
        Uuid::new_v4(),
        PollPolicy::default(),
        Box::new(ScalarOnly(Mock(device.clone()))),
        Arc::new(MonotonicClock::default()),
    )
    .unwrap();
    let owner = Uuid::new_v4();
    source.acquire(owner).await.unwrap();
    source.control(owner, true).await.unwrap();
    let generation = source.snapshot().generation;
    let budget = ImageBudget::new(12).unwrap();
    assert_eq!(
        source
            .camera_image_fenced(owner, generation, budget.clone(), Duration::from_secs(1))
            .await
            .err()
            .unwrap()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        source.read(owner, "issafe", Values::new()).await.unwrap(),
        json!(true)
    );
    assert_eq!(source.snapshot().generation, generation);
    assert_eq!(device.resets.load(SeqCst), 0);
    assert_eq!(device.downloads.load(SeqCst), 0);
    assert_eq!(budget.used_bytes(), 0);
    source.release(owner).await.unwrap();
    source.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn queued_download_cancelled_before_dispatch_never_reaches_backend() {
    let device = Arc::new(Device::default());
    device.hang_image.store(true, SeqCst);
    let source = spawn(&device);
    let owner = Uuid::new_v4();
    source.acquire(owner).await.unwrap();
    source.control(owner, true).await.unwrap();
    let generation = source.snapshot().generation;
    let budget = ImageBudget::new(12).unwrap();
    let start = || {
        let source = source.clone();
        let budget = budget.clone();
        tokio::spawn(async move {
            source
                .camera_image_fenced(owner, generation, budget, Duration::from_secs(1))
                .await
        })
    };
    let dispatched = start();
    settle().await;
    assert_eq!(device.downloads.load(SeqCst), 1);
    let queued = start();
    settle().await;
    queued.abort();
    assert!(matches!(queued.await, Err(error) if error.is_cancelled()));
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(
        dispatched.await.unwrap().err().unwrap().kind,
        ErrorKind::Transient
    );
    settle().await;
    assert_eq!(device.downloads.load(SeqCst), 1);
    assert_eq!(budget.used_bytes(), 0);
    source.release(owner).await.unwrap();
    source.shutdown().await.unwrap();
}
