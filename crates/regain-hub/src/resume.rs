//! Host-wide sleep/resume fencing. No equipment command is dispatched here.
use crate::{
    safety::{Clock, MonotonicClock},
    source::{ErrorKind, SourceError},
};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;

// Bracket the continuous read with awake-clock reads. Scheduling delays widen
// the possible gap rather than masquerading as sleep. Windows' coarse clocks
// need additional tolerance; its native power callback handles short sleeps.
#[cfg(windows)]
const CLOCK_TOLERANCE: Duration = Duration::from_millis(50);
#[cfg(not(windows))]
const CLOCK_TOLERANCE: Duration = Duration::from_nanos(1);
type ClockPair = (Duration, Duration, Duration);

#[derive(Default)]
struct Detector {
    gap: Option<(i128, i128)>,
    failed: bool,
}
impl Detector {
    fn observe(&mut self, pair: std::io::Result<ClockPair>) -> bool {
        let Ok((continuous, before, after)) = pair else {
            return !std::mem::replace(&mut self.failed, true);
        };
        if after < before {
            return !std::mem::replace(&mut self.failed, true);
        }
        let tolerance = CLOCK_TOLERANCE.as_nanos() as i128;
        let gap = (
            continuous.as_nanos() as i128 - after.as_nanos() as i128 - tolerance,
            continuous.as_nanos() as i128 - before.as_nanos() as i128 + tolerance,
        );
        let intersection = self.gap.map(|old| (old.0.max(gap.0), old.1.min(gap.1)));
        let changed = intersection.is_some_and(|range| range.0 > range.1);
        let recovered = std::mem::replace(&mut self.failed, false);
        self.gap = if changed || recovered {
            Some(gap)
        } else {
            intersection.or(Some(gap))
        };
        changed || recovered
    }
}

pub struct ResumeClock {
    clock: Arc<dyn Clock>,
    epochs: Arc<watch::Sender<u64>>,
    detector: Option<Mutex<Detector>>,
    monitor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl ResumeClock {
    /// Embedded hosts may deliver their own OS notifications. This constructor
    /// never registers with the OS and is also useful for deterministic tests.
    pub fn manual(clock: Arc<dyn Clock>) -> Arc<Self> {
        let (epochs, _) = watch::channel(0);
        Arc::new(Self {
            clock,
            epochs: Arc::new(epochs),
            detector: None,
            monitor: Mutex::new(None),
        })
    }
    /// Install the production monitor before building any source/runtime. One
    /// clock is shared across configuration replacements, so Apply cannot miss
    /// a notification while an unpublished candidate is being constructed.
    pub fn start() -> std::io::Result<Arc<Self>> {
        let pair = platform_pair()?;
        #[cfg(windows)]
        let epochs = windows_epochs()?;
        #[cfg(not(windows))]
        let epochs = Arc::new(watch::channel(0).0);
        let mut detector = Detector::default();
        detector.observe(Ok(pair));
        let clock = Arc::new(Self {
            clock: Arc::new(MonotonicClock::default()),
            epochs,
            detector: Some(Mutex::new(detector)),
            monitor: Mutex::new(None),
        });
        let weak = Arc::downgrade(&clock);
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(250));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(clock) = weak.upgrade() else {
                    break;
                };
                clock.resume_epoch();
            }
        });
        *clock.monitor.lock().unwrap() = Some(task);
        Ok(clock)
    }
    pub fn notify_resume(&self) {
        advance(&self.epochs);
    }
}
fn advance(epochs: &watch::Sender<u64>) {
    epochs.send_modify(|epoch| *epoch = epoch.checked_add(1).expect("Resume epoch exhausted"));
}
impl Clock for ResumeClock {
    fn now(&self) -> Duration {
        self.clock.now()
    }
    fn resume_epoch(&self) -> u64 {
        if let Some(detector) = &self.detector {
            // Serialize the OS reads as well: concurrent samples must not look
            // like a backwards clock discontinuity when completed out of order.
            if detector.lock().unwrap().observe(platform_pair()) {
                self.notify_resume();
            }
        }
        *self.epochs.borrow()
    }
    fn resume_notifications(&self) -> Option<watch::Receiver<u64>> {
        Some(self.epochs.subscribe())
    }
    fn resume_clock_valid(&self) -> bool {
        self.detector
            .as_ref()
            .is_none_or(|detector| !detector.lock().unwrap().failed)
    }
}
impl Drop for ResumeClock {
    fn drop(&mut self) {
        if let Some(task) = self.monitor.get_mut().unwrap().take() {
            task.abort();
        }
    }
}

pub(crate) fn interrupted() -> SourceError {
    SourceError {
        transport_lost: true,
        ..SourceError::new(
            ErrorKind::Unavailable,
            "Host resumed or its sleep clock changed; the old request was discarded",
        )
    }
}

/// Cancel local I/O on notification and check again after completion. The
/// caller decides whether an interrupted mutation has an uncertain outcome.
pub(crate) async fn fence<T>(
    clock: &dyn Clock,
    epoch: u64,
    future: impl Future<Output = Result<T, SourceError>>,
) -> Result<T, SourceError> {
    let mut notifications = clock.resume_notifications();
    if clock.resume_epoch() != epoch || !clock.resume_clock_valid() {
        return Err(interrupted());
    }
    let result = tokio::select! {
        biased;
        _ = changed(&mut notifications) => Err(interrupted()),
        result = future => result,
    };
    if clock.resume_epoch() != epoch || !clock.resume_clock_valid() {
        Err(interrupted())
    } else {
        result
    }
}
pub(crate) async fn changed(notifications: &mut Option<watch::Receiver<u64>>) {
    match notifications {
        Some(receiver) => {
            let _ = receiver.changed().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn platform_pair() -> std::io::Result<ClockPair> {
    fn read(id: libc::clockid_t) -> std::io::Result<Duration> {
        let mut value = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: the OS initializes a valid timespec on success.
        if unsafe { libc::clock_gettime(id, value.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let value = unsafe { value.assume_init() };
        if value.tv_sec < 0 || !(0..1_000_000_000).contains(&value.tv_nsec) {
            return Err(std::io::Error::other("Invalid sleep clock"));
        }
        Ok(Duration::new(value.tv_sec as u64, value.tv_nsec as u32))
    }
    #[cfg(target_os = "linux")]
    let ids = (libc::CLOCK_BOOTTIME, libc::CLOCK_MONOTONIC);
    #[cfg(target_os = "macos")]
    let ids = (libc::CLOCK_MONOTONIC_RAW, libc::CLOCK_UPTIME_RAW);
    let before = read(ids.1)?;
    Ok((read(ids.0)?, before, read(ids.1)?))
}
#[cfg(windows)]
fn platform_pair() -> std::io::Result<ClockPair> {
    use windows_sys::Win32::System::WindowsProgramming::{
        QueryInterruptTime, QueryUnbiasedInterruptTime,
    };
    let (mut continuous, mut before, mut after) = (0, 0, 0);
    // SAFETY: both functions write into initialized u64 storage.
    unsafe {
        if QueryUnbiasedInterruptTime(&mut before) == 0 {
            return Err(std::io::Error::last_os_error());
        }
        QueryInterruptTime(&mut continuous);
        if QueryUnbiasedInterruptTime(&mut after) == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok((
        Duration::from_nanos(continuous.saturating_mul(100)),
        Duration::from_nanos(before.saturating_mul(100)),
        Duration::from_nanos(after.saturating_mul(100)),
    ))
}
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn platform_pair() -> std::io::Result<ClockPair> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Host sleep clock is unsupported",
    ))
}

#[cfg(windows)]
fn windows_epochs() -> std::io::Result<Arc<watch::Sender<u64>>> {
    use std::{ffi::c_void, sync::OnceLock};
    use windows_sys::Win32::{
        System::Power::{
            DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, PowerRegisterSuspendResumeNotification,
        },
        UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND},
    };
    static EPOCHS: OnceLock<Arc<watch::Sender<u64>>> = OnceLock::new();
    static REGISTRATION: OnceLock<Result<usize, u32>> = OnceLock::new();
    unsafe extern "system" fn callback(_: *const c_void, event: u32, _: *const c_void) -> u32 {
        if matches!(event, PBT_APMSUSPEND | PBT_APMRESUMEAUTOMATIC) {
            // No borrowed context or equipment access. The callback and its
            // sender deliberately live for the process lifetime; Windows owns
            // registration cleanup at exit, avoiding unregister/callback races.
            let _ = std::panic::catch_unwind(|| {
                if let Some(epochs) = EPOCHS.get() {
                    advance(epochs);
                }
            });
        }
        0
    }
    let epochs = EPOCHS.get_or_init(|| Arc::new(watch::channel(0).0));
    match REGISTRATION.get_or_init(|| {
        let mut parameters = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(callback),
            Context: std::ptr::null_mut(),
        };
        let mut registration = std::ptr::null_mut();
        // SAFETY: registration copies the callback/context; neither references
        // stack storage. A single process-lifetime registration is retained.
        let error = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                (&mut parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS).cast(),
                &mut registration,
            )
        };
        if error == 0 {
            Ok(registration as usize)
        } else {
            Err(error)
        }
    }) {
        Ok(_) => Ok(epochs.clone()),
        Err(error) => Err(std::io::Error::from_raw_os_error(*error as i32)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn brackets_ignore_scheduling_delay_but_detect_sleep_and_clock_faults() {
        let mut detector = Detector::default();
        let pair = |continuous, before, after| {
            Ok((
                Duration::from_millis(continuous),
                Duration::from_millis(before),
                Duration::from_millis(after),
            ))
        };
        assert!(!detector.observe(pair(10_000, 9_900, 10_100)));
        assert!(!detector.observe(pair(11_000, 10_950, 11_050)));
        assert!(detector.observe(pair(12_000, 10_900, 11_100)));
        assert!(!detector.observe(pair(13_000, 11_950, 12_050)));
        assert!(detector.observe(Err(std::io::Error::other("clock"))));
        assert!(!detector.observe(Err(std::io::Error::other("clock"))));
        assert!(detector.observe(pair(20_000, 19_000, 19_000)));
        assert!(detector.observe(pair(20_000, 20_000, 20_000)));
        assert!(detector.observe(pair(20_000, 20_001, 20_000)));
    }
    #[cfg(not(windows))]
    #[test]
    fn short_sleeps_are_not_hidden_by_a_fixed_millisecond_threshold() {
        let mut detector = Detector::default();
        let awake = Duration::from_secs(10);
        assert!(!detector.observe(Ok((awake, awake, awake))));
        assert!(detector.observe(Ok((awake + Duration::from_micros(1), awake, awake))));
    }
    #[tokio::test]
    async fn production_monitor_starts_without_equipment_or_sleep() {
        let clock = ResumeClock::start().unwrap();
        let before = clock.resume_epoch();
        clock.notify_resume();
        assert!(clock.resume_epoch() > before);
    }
    #[tokio::test]
    async fn failed_monitor_blocks_io_even_without_an_additional_epoch_change() {
        struct FailedClock;
        impl Clock for FailedClock {
            fn now(&self) -> Duration {
                Duration::ZERO
            }
            fn resume_clock_valid(&self) -> bool {
                false
            }
        }
        let mut polled = false;
        let result = fence(&FailedClock, 0, async {
            polled = true;
            Ok(())
        })
        .await;
        assert_eq!(result.unwrap_err().kind, ErrorKind::Unavailable);
        assert!(!polled);
    }
}
