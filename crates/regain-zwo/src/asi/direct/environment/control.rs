//! One bounded cooling request, executed only at USB-owner service checkpoints.
use std::{
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoolingError {
    Busy,
    Expired,
    Closed,
    Uncertain(String),
}
impl std::fmt::Display for CoolingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("Another cooling request is pending"),
            Self::Expired => f.write_str("Cooling request expired before USB dispatch"),
            Self::Closed => f.write_str("Cooling owner has retired"),
            Self::Uncertain(message) => write!(f, "Cooling outcome is uncertain: {message}"),
        }
    }
}
impl std::error::Error for CoolingError {}
enum Phase {
    Queued,
    Running,
    Finished {
        at: Instant,
        result: Result<i64, CoolingError>,
    },
}
struct Request {
    control: u32,
    value: i64,
    deadline: Instant,
    phase: Mutex<Phase>,
    changed: Condvar,
}
#[derive(Default)]
struct State {
    pending: Option<Arc<Request>>,
    closed: bool,
}
#[derive(Clone, Default)]
pub struct CoolingQueue(Arc<Mutex<State>>);
pub struct Owner(CoolingQueue);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.retire();
    }
}

impl CoolingQueue {
    pub fn owner(&self) -> Owner {
        Owner(self.clone())
    }
    /// One command slot, including dispatched work whose waiter timed out.
    /// The caller validates hardware capability/range before admission.
    pub fn call(&self, control: u32, value: i64, timeout: Duration) -> Result<i64, CoolingError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or(CoolingError::Expired)?;
        let request = Arc::new(Request {
            control,
            value,
            deadline,
            phase: Mutex::new(Phase::Queued),
            changed: Condvar::new(),
        });
        {
            let mut state = self.0.lock().unwrap();
            if state.closed {
                return Err(CoolingError::Closed);
            }
            if state.pending.is_some() {
                return Err(CoolingError::Busy);
            }
            state.pending = Some(request.clone());
        }
        let mut phase = request.phase.lock().unwrap();
        loop {
            if let Phase::Finished { at, result } = &*phase {
                return if *at > deadline && result.is_ok() {
                    Err(CoolingError::Uncertain(
                        "Acknowledgement exceeded its deadline".into(),
                    ))
                } else {
                    result.clone()
                };
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                if matches!(*phase, Phase::Queued) {
                    *phase = Phase::Finished {
                        at: Instant::now(),
                        result: Err(CoolingError::Expired),
                    };
                    drop(phase);
                    self.release(&request);
                    return Err(CoolingError::Expired);
                }
                // Do not free the slot or cancel an already dispatched write.
                return Err(CoolingError::Uncertain(
                    "USB dispatch has no acknowledgement".into(),
                ));
            }
            phase = request.changed.wait_timeout(phase, remaining).unwrap().0;
        }
    }
    fn release(&self, request: &Arc<Request>) {
        let mut state = self.0.lock().unwrap();
        if state
            .pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, request))
        {
            state.pending = None;
        }
    }
    /// Only the transport owner calls this; it never holds the queue lock for USB.
    pub fn service(&self, apply: impl FnOnce(u32, i64) -> anyhow::Result<i64>) {
        let Some(request) = self.0.lock().unwrap().pending.clone() else {
            return;
        };
        {
            let mut phase = request.phase.lock().unwrap();
            match &*phase {
                Phase::Queued if Instant::now() < request.deadline => *phase = Phase::Running,
                Phase::Queued => {
                    *phase = Phase::Finished {
                        at: Instant::now(),
                        result: Err(CoolingError::Expired),
                    };
                    request.changed.notify_all();
                    drop(phase);
                    self.release(&request);
                    return;
                }
                Phase::Finished { .. } => {
                    drop(phase);
                    self.release(&request);
                    return;
                }
                Phase::Running => return,
            }
        }
        let result = apply(request.control, request.value).map_err(|error| {
            let mut message = format!("{error:#}");
            let mut end = message.len().min(2048);
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            CoolingError::Uncertain(message)
        });
        let mut phase = request.phase.lock().unwrap();
        if matches!(*phase, Phase::Finished { .. }) {
            // Retirement already published uncertainty; a late completion cannot
            // turn that into a known acknowledgement.
            return;
        }
        let at = Instant::now();
        let result = if at > request.deadline && result.is_ok() {
            Err(CoolingError::Uncertain(
                "Acknowledgement exceeded its deadline".into(),
            ))
        } else {
            result
        };
        if result.is_err() {
            // No additional write may follow an unknown hardware outcome.
            self.0.lock().unwrap().closed = true;
        }
        self.release(&request);
        *phase = Phase::Finished { at, result };
        request.changed.notify_all();
    }
    fn retire(&self) {
        let pending = {
            let mut state = self.0.lock().unwrap();
            state.closed = true;
            state.pending.take()
        };
        if let Some(request) = pending {
            let mut phase = request.phase.lock().unwrap();
            let error = match *phase {
                Phase::Queued => CoolingError::Closed,
                Phase::Running => {
                    CoolingError::Uncertain("USB owner retired during dispatch".into())
                }
                Phase::Finished { .. } => return,
            };
            *phase = Phase::Finished {
                at: Instant::now(),
                result: Err(error),
            };
            request.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending(queue: &CoolingQueue) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while queue.0.lock().unwrap().pending.is_none() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn known_acknowledgement_and_before_dispatch_expiry() {
        let queue = CoolingQueue::default();
        let caller = queue.clone();
        let task = std::thread::spawn(move || caller.call(16, -10, Duration::from_secs(2)));
        pending(&queue);
        queue.service(|control, value| {
            assert_eq!((control, value), (16, -10));
            Ok(value)
        });
        assert_eq!(task.join().unwrap(), Ok(-10));
        assert_eq!(
            queue.call(17, 0, Duration::from_millis(1)),
            Err(CoolingError::Expired)
        );
        queue.service(|_, _| panic!("Expired cooling request must not reach USB"));
        assert!(queue.0.lock().unwrap().pending.is_none());
    }
    #[test]
    fn timed_out_dispatch_retains_slot_until_owner_finishes() {
        let queue = CoolingQueue::default();
        let caller = queue.clone();
        let task = std::thread::spawn(move || caller.call(17, 0, Duration::from_secs(1)));
        pending(&queue);
        let worker = queue.clone();
        let (entered, seen) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let service = std::thread::spawn(move || {
            worker.service(|_, value| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                Ok(value)
            })
        });
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            task.join().unwrap(),
            Err(CoolingError::Uncertain(_))
        ));
        assert_eq!(
            queue.call(16, -10, Duration::from_secs(1)),
            Err(CoolingError::Busy)
        );
        release.send(()).unwrap();
        service.join().unwrap();
        assert!(queue.0.lock().unwrap().pending.is_none());
        assert_eq!(
            queue.call(16, -10, Duration::from_secs(1)),
            Err(CoolingError::Closed)
        );
    }
    #[test]
    fn owner_loss_and_usb_failures_are_distinct_from_unsent_expiry() {
        let queue = CoolingQueue::default();
        let owner = queue.owner();
        let caller = queue.clone();
        let task = std::thread::spawn(move || caller.call(16, -10, Duration::from_secs(2)));
        pending(&queue);
        drop(owner);
        assert_eq!(task.join().unwrap(), Err(CoolingError::Closed));
        assert_eq!(
            queue.call(17, 0, Duration::from_secs(1)),
            Err(CoolingError::Closed)
        );
        let queue = CoolingQueue::default();
        let caller = queue.clone();
        let task = std::thread::spawn(move || caller.call(17, 0, Duration::from_secs(2)));
        pending(&queue);
        queue.service(|_, _| anyhow::bail!("private USB failure"));
        assert!(
            matches!(task.join().unwrap(),Err(CoolingError::Uncertain(message)) if message=="private USB failure")
        );
        assert_eq!(
            queue.call(17, 0, Duration::from_secs(1)),
            Err(CoolingError::Closed)
        );
    }

    #[test]
    fn retired_owner_cannot_publish_a_late_known_acknowledgement() {
        let queue = CoolingQueue::default();
        let owner = queue.owner();
        let caller = queue.clone();
        let task = std::thread::spawn(move || caller.call(17, 0, Duration::from_secs(5)));
        pending(&queue);
        let worker = queue.clone();
        let (entered, seen) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let service = std::thread::spawn(move || {
            worker.service(|_, value| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                Ok(value)
            })
        });
        seen.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(owner);
        assert!(matches!(
            task.join().unwrap(),
            Err(CoolingError::Uncertain(_))
        ));
        release.send(()).unwrap();
        service.join().unwrap();
        assert_eq!(
            queue.call(16, -10, Duration::from_secs(1)),
            Err(CoolingError::Closed)
        );
    }
}
