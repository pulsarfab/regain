//! One acknowledged cooler command, driven by the existing Session owner.
//! The handle grants no equipment lease; frontends must authorize callers.
use crate::SharedStatus;
use std::sync::{Arc, Mutex};
use tokio::{
    sync::oneshot,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoolingError {
    Busy,
    Unavailable,
    Expired,
    Cancelled,
    Invalid(String),
    Uncertain { message: String, code: Option<i32> },
}

impl std::fmt::Display for CoolingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => f.write_str("Another cooler request is pending"),
            Self::Unavailable => f.write_str("Camera controls are unavailable"),
            Self::Expired => f.write_str("Cooler request expired before dispatch"),
            Self::Cancelled => f.write_str("Cooler request was cancelled before dispatch"),
            Self::Invalid(message) => f.write_str(message),
            Self::Uncertain { message, .. } => write!(f, "Cooler outcome is uncertain: {message}"),
        }
    }
}
impl std::error::Error for CoolingError {}
type Outcome = Result<i64, CoolingError>;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Queued,
    Claimed,
    Running,
    Finished,
}
pub(crate) struct Request {
    pub kind: i32,
    pub value: i64,
    pub deadline: Instant,
    phase: Phase,
    uncertain: bool,
    reply: Option<oneshot::Sender<Outcome>>,
}
#[derive(Default)]
struct State {
    accepting: bool,
    pending: Option<Arc<Mutex<Request>>>,
}
#[derive(Clone, Default)]
pub(crate) struct Mailbox(Arc<Mutex<State>>);
#[derive(Clone)]
pub struct CoolingHandle {
    pub(crate) mailbox: Mailbox,
    pub(crate) status: SharedStatus,
}
pub struct CoolingReceipt {
    mailbox: Mailbox,
    request: Arc<Mutex<Request>>,
    reply: oneshot::Receiver<Outcome>,
    deadline: Instant,
}
/// Withdraw an unsent request without dropping its owner's receipt. Once
/// dispatched this is inert: the owner must retain the command to its outcome.
#[derive(Clone)]
pub struct CoolingCancellation {
    mailbox: Mailbox,
    request: Arc<Mutex<Request>>,
}
impl CoolingCancellation {
    pub fn cancel_before_dispatch(&self) {
        self.mailbox.cancel_queued(&self.request);
    }
}
pub(crate) fn validate(status: &SharedStatus, kind: i32, value: i64) -> Result<(), CoolingError> {
    if !matches!(kind, 16 | 17) {
        return Err(CoolingError::Invalid(
            "Only cooler target and enable are supported".into(),
        ));
    }
    let state = status.lock().unwrap();
    if !state.connected || !state.control_connection_available {
        return Err(CoolingError::Unavailable);
    }
    let cap = state
        .controls
        .get(&kind)
        .ok_or_else(|| CoolingError::Invalid("Cooler control is unavailable".into()))?;
    if !cap.writable || value < cap.min || value > cap.max {
        return Err(CoolingError::Invalid(format!(
            "Cooler control must be writable and between {} and {}",
            cap.min, cap.max
        )));
    }
    Ok(())
}
impl CoolingHandle {
    /// Reserve synchronously; await the receipt while the owner drives Session.
    /// The deadline covers queueing, write and readback, not thermal convergence.
    pub fn submit(
        &self,
        kind: i32,
        value: i64,
        timeout: Duration,
    ) -> Result<CoolingReceipt, CoolingError> {
        validate(&self.status, kind, value)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or(CoolingError::Expired)?;
        let (sender, reply) = oneshot::channel();
        let request = Arc::new(Mutex::new(Request {
            kind,
            value,
            deadline,
            phase: Phase::Queued,
            uncertain: false,
            reply: Some(sender),
        }));
        let mut state = self.mailbox.0.lock().unwrap();
        if !state.accepting {
            return Err(CoolingError::Unavailable);
        }
        if state.pending.is_some() {
            return Err(CoolingError::Busy);
        }
        state.pending = Some(request.clone());
        Ok(CoolingReceipt {
            mailbox: self.mailbox.clone(),
            request,
            reply,
            deadline,
        })
    }
    pub fn pending(&self) -> bool {
        self.mailbox.0.lock().unwrap().pending.is_some()
    }
}
impl CoolingReceipt {
    pub fn cancellation(&self) -> CoolingCancellation {
        CoolingCancellation {
            mailbox: self.mailbox.clone(),
            request: self.request.clone(),
        }
    }
    pub async fn wait(mut self) -> Outcome {
        tokio::select! {
            biased;
            result=&mut self.reply=>result.unwrap_or(Err(CoolingError::Unavailable)),
            _=tokio::time::sleep_until(self.deadline)=>{
                if let Some(error)=self.mailbox.expire(&self.request) {return Err(error);}
                // Completion and expiry raced under one lock. Completion already
                // sent its outcome, including an acknowledgement before deadline.
                (&mut self.reply).await.unwrap_or(Err(CoolingError::Unavailable))
            }
        }
    }
}
impl Drop for CoolingReceipt {
    fn drop(&mut self) {
        self.mailbox.cancel_queued(&self.request);
    }
}
impl Mailbox {
    pub fn activate(&self) {
        self.retire();
        self.0.lock().unwrap().accepting = true;
    }
    pub fn retire(&self) {
        let mut state = self.0.lock().unwrap();
        state.accepting = false;
        if let Some(request) = state.pending.take() {
            let mut request = request.lock().unwrap();
            let result = if request.phase == Phase::Running {
                Err(CoolingError::Uncertain {
                    message: "Camera owner retired during cooler dispatch".into(),
                    code: None,
                })
            } else {
                Err(CoolingError::Unavailable)
            };
            request.phase = Phase::Finished;
            if let Some(reply) = request.reply.take() {
                let _ = reply.send(result);
            }
        }
    }
    fn cancel_queued(&self, request: &Arc<Mutex<Request>>) {
        let mut state = self.0.lock().unwrap();
        if !state
            .pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, request))
        {
            return;
        }
        let mut request = request.lock().unwrap();
        if matches!(request.phase, Phase::Queued | Phase::Claimed) {
            request.phase = Phase::Finished;
            state.pending = None;
            if let Some(reply) = request.reply.take() {
                let _ = reply.send(Err(CoolingError::Cancelled));
            }
        }
    }
    fn expire(&self, request: &Arc<Mutex<Request>>) -> Option<CoolingError> {
        let mut state = self.0.lock().unwrap();
        if !state
            .pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, request))
        {
            return None;
        }
        let mut request = request.lock().unwrap();
        if matches!(request.phase, Phase::Queued | Phase::Claimed) {
            request.phase = Phase::Finished;
            state.pending = None;
            let error = CoolingError::Expired;
            if let Some(reply) = request.reply.take() {
                let _ = reply.send(Err(error.clone()));
            }
            Some(error)
        } else {
            // Keep the slot and dispatched ownership until the Session finishes.
            request.uncertain = true;
            state.accepting = false;
            Some(CoolingError::Uncertain {
                message: "Cooler acknowledgement exceeded its deadline".into(),
                code: None,
            })
        }
    }
    pub fn claim(&self) -> Option<Arc<Mutex<Request>>> {
        let mut state = self.0.lock().unwrap();
        let request = state.pending.clone()?;
        let mut command = request.lock().unwrap();
        if command.phase != Phase::Queued {
            return None;
        }
        if Instant::now() >= command.deadline {
            command.phase = Phase::Finished;
            state.pending = None;
            if let Some(reply) = command.reply.take() {
                let _ = reply.send(Err(CoolingError::Expired));
            }
            return None;
        }
        command.phase = Phase::Claimed;
        drop(command);
        Some(request)
    }
    pub fn dispatch(&self, request: &Arc<Mutex<Request>>) -> bool {
        let mut state = self.0.lock().unwrap();
        if !state
            .pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, request))
        {
            return false;
        }
        let mut command = request.lock().unwrap();
        if command.phase != Phase::Claimed || !state.accepting {
            return false;
        }
        if Instant::now() >= command.deadline {
            command.phase = Phase::Finished;
            state.pending = None;
            if let Some(reply) = command.reply.take() {
                let _ = reply.send(Err(CoolingError::Expired));
            }
            return false;
        }
        command.phase = Phase::Running;
        true
    }
    /// Commit shared/core recovery state before publishing a known outcome.
    /// Neither mutex is held over worker I/O.
    pub fn finish(
        &self,
        request: &Arc<Mutex<Request>>,
        mut result: Outcome,
        commit: impl FnOnce(),
    ) -> Outcome {
        let mut state = self.0.lock().unwrap();
        let current = state
            .pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, request));
        let mut command = request.lock().unwrap();
        if !current || !matches!(command.phase, Phase::Claimed | Phase::Running) {
            return Err(CoolingError::Uncertain {
                message: "Cooler owner changed before acknowledgement".into(),
                code: None,
            });
        }
        if !matches!(result, Err(CoolingError::Uncertain { .. }))
            && (command.uncertain
                || !state.accepting
                || (result.is_ok()
                    && (command.phase != Phase::Running || Instant::now() > command.deadline)))
        {
            result = Err(CoolingError::Uncertain {
                message: "Cooler acknowledgement exceeded its deadline".into(),
                code: None,
            });
        }
        if matches!(result, Err(CoolingError::Uncertain { .. })) {
            state.accepting = false;
        }
        if result.is_ok() {
            commit();
        }
        command.phase = Phase::Finished;
        state.pending = None;
        if let Some(reply) = command.reply.take() {
            let _ = reply.send(result.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Control, Status};
    fn handle() -> CoolingHandle {
        let status = Status {
            connected: true,
            control_connection_available: true,
            controls: [
                (
                    16,
                    Control {
                        kind: 16,
                        min: -40,
                        max: 30,
                        value: 0,
                        writable: true,
                    },
                ),
                (
                    17,
                    Control {
                        kind: 17,
                        min: 0,
                        max: 1,
                        value: 0,
                        writable: true,
                    },
                ),
            ]
            .into(),
            values: [(16, 0), (17, 0)].into(),
            ..Status::default()
        };
        let handle = CoolingHandle {
            mailbox: Mailbox::default(),
            status: Arc::new(Mutex::new(status)),
        };
        handle.mailbox.activate();
        handle
    }
    fn dispatch(handle: &CoolingHandle) -> Arc<Mutex<Request>> {
        let request = handle.mailbox.claim().unwrap();
        assert!(handle.mailbox.dispatch(&request));
        request
    }
    #[tokio::test]
    async fn bounded_admission_validation_and_known_acknowledgement() {
        let handle = handle();
        for (kind, value) in [(0, 1), (16, -41), (16, 31), (17, 2)] {
            assert!(matches!(
                handle.submit(kind, value, Duration::from_secs(1)),
                Err(CoolingError::Invalid(_))
            ));
        }
        let receipt = handle.submit(16, -10, Duration::from_secs(5)).unwrap();
        assert!(matches!(
            handle.submit(17, 1, Duration::from_secs(5)),
            Err(CoolingError::Busy)
        ));
        assert_eq!(handle.status.lock().unwrap().values[&16], 0);
        let request = dispatch(&handle);
        handle
            .mailbox
            .finish(&request, Ok(-10), || {
                handle.status.lock().unwrap().values.insert(16, -10);
            })
            .unwrap();
        assert_eq!(receipt.wait().await, Ok(-10));
        assert!(!handle.pending());
        assert_eq!(handle.status.lock().unwrap().values[&16], -10);
    }
    #[tokio::test]
    async fn queued_expiry_and_waiter_loss_never_dispatch() {
        let handle = handle();
        drop(handle.submit(17, 1, Duration::from_secs(5)).unwrap());
        assert!(!handle.pending());
        assert!(handle.mailbox.claim().is_none());
        let receipt = handle.submit(16, -10, Duration::from_millis(1)).unwrap();
        assert_eq!(receipt.wait().await, Err(CoolingError::Expired));
        assert!(handle.mailbox.claim().is_none());
        assert!(!handle.pending());
    }
    #[tokio::test]
    async fn dispatched_waiter_loss_keeps_owned_commit() {
        let handle = handle();
        let receipt = handle.submit(17, 1, Duration::from_secs(5)).unwrap();
        let request = dispatch(&handle);
        drop(receipt);
        assert!(handle.pending());
        handle
            .mailbox
            .finish(&request, Ok(1), || {
                handle.status.lock().unwrap().values.insert(17, 1);
            })
            .unwrap();
        assert!(!handle.pending());
        assert_eq!(handle.status.lock().unwrap().values[&17], 1);
    }
    #[tokio::test]
    async fn dispatched_deadline_fences_and_cannot_commit_late_or_after_reactivation() {
        let handle = handle();
        let mut receipt = handle.submit(16, -10, Duration::from_secs(5)).unwrap();
        let request = dispatch(&handle);
        // Put the already-dispatched command exactly at its deadline without a
        // scheduling-sensitive window between submission and claim.
        let deadline = Instant::now();
        request.lock().unwrap().deadline = deadline;
        receipt.deadline = deadline;
        assert!(matches!(
            receipt.wait().await,
            Err(CoolingError::Uncertain { .. })
        ));
        assert!(handle.pending());
        assert!(matches!(
            handle.submit(17, 1, Duration::from_secs(1)),
            Err(CoolingError::Unavailable)
        ));
        assert!(matches!(
            handle.mailbox.finish(&request, Ok(-10), || panic!(
                "Late cooler result must not commit"
            )),
            Err(CoolingError::Uncertain { .. })
        ));
        handle.mailbox.activate();
        let next = handle.submit(16, -15, Duration::from_secs(5)).unwrap();
        assert!(matches!(
            handle.mailbox.finish(&request, Ok(-10), || panic!(
                "Old cooler result must not commit"
            )),
            Err(CoolingError::Uncertain { .. })
        ));
        assert!(handle.pending());
        drop(next);
        assert!(!handle.pending());
    }
    #[tokio::test]
    async fn retirement_distinguishes_unsent_and_dispatched_and_preserves_codes() {
        let handle = handle();
        let queued = handle.submit(16, -10, Duration::from_secs(5)).unwrap();
        handle.mailbox.retire();
        assert_eq!(queued.wait().await, Err(CoolingError::Unavailable));
        handle.mailbox.activate();
        let running = handle.submit(17, 1, Duration::from_secs(5)).unwrap();
        let request = dispatch(&handle);
        handle.mailbox.retire();
        assert!(matches!(
            running.wait().await,
            Err(CoolingError::Uncertain { .. })
        ));
        assert!(matches!(
            handle
                .mailbox
                .finish(&request, Ok(1), || panic!("Retired cooler must not commit")),
            Err(CoolingError::Uncertain { .. })
        ));
        handle.mailbox.activate();
        let receipt = handle.submit(16, -10, Duration::from_secs(5)).unwrap();
        let request = dispatch(&handle);
        let error = CoolingError::Uncertain {
            message: "private failure".into(),
            code: Some(11),
        };
        assert_eq!(
            handle
                .mailbox
                .finish(&request, Err(error.clone()), || panic!(
                    "Failed cooler must not commit"
                )),
            Err(error.clone())
        );
        assert_eq!(receipt.wait().await, Err(error));
        assert!(!handle.pending());
        assert!(matches!(
            handle.submit(17, 1, Duration::from_secs(1)),
            Err(CoolingError::Unavailable)
        ));
    }
    #[tokio::test]
    async fn caller_loss_and_deadline_during_preflight_skip_dispatch() {
        let handle = handle();
        let receipt = handle.submit(16, -10, Duration::from_secs(5)).unwrap();
        let request = handle.mailbox.claim().unwrap();
        drop(receipt);
        assert!(!handle.mailbox.dispatch(&request));
        assert!(!handle.pending());
        let mut receipt = handle.submit(17, 1, Duration::from_secs(5)).unwrap();
        let request = handle.mailbox.claim().unwrap();
        let deadline = Instant::now();
        request.lock().unwrap().deadline = deadline;
        receipt.deadline = deadline;
        assert_eq!(receipt.wait().await, Err(CoolingError::Expired));
        assert!(!handle.mailbox.dispatch(&request));
        assert!(!handle.pending());
    }
    #[tokio::test]
    async fn separate_caller_cancellation_preserves_the_owned_receipt() {
        let handle = handle();
        for claimed in [false, true] {
            let receipt = handle.submit(16, -10, Duration::from_secs(5)).unwrap();
            let cancellation = receipt.cancellation();
            let request = claimed.then(|| handle.mailbox.claim().unwrap());
            cancellation.cancel_before_dispatch();
            assert_eq!(receipt.wait().await, Err(CoolingError::Cancelled));
            if let Some(request) = request {
                assert!(!handle.mailbox.dispatch(&request));
            }
            assert!(!handle.pending());
        }
        let receipt = handle.submit(17, 1, Duration::from_secs(5)).unwrap();
        let cancellation = receipt.cancellation();
        let request = dispatch(&handle);
        cancellation.cancel_before_dispatch();
        assert!(handle.pending());
        handle.mailbox.finish(&request, Ok(1), || {}).unwrap();
        assert_eq!(receipt.wait().await, Ok(1));
        // An old caller must never cancel a later slot.
        let next = handle.submit(16, -15, Duration::from_secs(5)).unwrap();
        cancellation.cancel_before_dispatch();
        assert!(handle.pending());
        drop(next);
    }
}
