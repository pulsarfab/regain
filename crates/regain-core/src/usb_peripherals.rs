//! Explicit frontend confirmation after a camera USB reset. The mailbox never
//! takes the camera engine lock, so a capture can await NINA while stdin remains
//! available to acknowledge the matching recovery episode.
use crate::{CancellationToken, Failure, SharedStatus, invalid};
use anyhow::{Result, ensure};
use serde::Deserialize;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::oneshot, time::Instant};

pub const WAIT_SECONDS: u64 = 120;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn next_id() -> Result<u64> {
    // Unlike a per-Session counter, this cannot reuse an episode on reconnect.
    // UTC microseconds also separate supervisor restarts; lifecycle records
    // carry the supervisor PID so frontends can reject old-process records.
    let seed = chrono::Utc::now().timestamp_micros().max(1) as u64;
    let mut previous = NEXT_ID.load(Ordering::Relaxed);
    loop {
        let next = previous
            .max(seed)
            .checked_add(1)
            .ok_or_else(|| invalid("USB peripheral recovery ID space exhausted"))?;
        match NEXT_ID.compare_exchange_weak(previous, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(next),
            Err(current) => previous = current,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acknowledgement {
    pub id: u64,
    pub success: bool,
    pub error: Option<String>,
}

struct Pending {
    id: u64,
    deadline: Option<Instant>,
    reply: Option<oneshot::Sender<Acknowledgement>>,
}
#[derive(Default)]
struct State {
    enabled: bool,
    retired: bool,
    pending: Option<Pending>,
    blocked: Option<String>,
}

#[derive(Clone)]
pub struct PeripheralRecoveryHandle {
    state: Arc<Mutex<State>>,
    status: SharedStatus,
}
impl PeripheralRecoveryHandle {
    pub(crate) fn new(status: SharedStatus) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            status,
        }
    }
    pub(crate) fn enable(&self, enabled: bool) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        ensure!(
            !state.retired && state.pending.is_none() && state.blocked.is_none(),
            invalid("Cannot change USB peripheral recovery during or after recovery")
        );
        state.enabled = enabled;
        Ok(())
    }
    /// Reject captures after an unconfirmed reset, including a later Start
    /// request. An explicit disconnect/reconnect clears this safety fence.
    pub(crate) fn ensure_ready(&self) -> Result<()> {
        let state = self.state.lock().unwrap();
        ensure!(!state.retired, invalid("Camera disconnected"));
        if let Some(message) = &state.blocked {
            return Err(invalid(message.clone()));
        }
        ensure!(
            state.pending.is_none(),
            invalid("USB peripheral recovery is still pending")
        );
        Ok(())
    }
    /// The Hub retains a Session across explicit disconnect/connect. Retiring
    /// it rejects stale handles, while a subsequent explicit connect may clear
    /// the fence. Calling connect alone during a failed episode cannot do so.
    pub(crate) fn reconnect_after_close(&self) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap();
            if state.retired {
                state.retired = false;
                state.blocked = None;
            }
        }
        self.ensure_ready()
    }
    /// Reserve the episode before reset dispatch; publish pending status only
    /// after camera reappearance when the frontend may reconnect peripherals.
    pub(crate) fn begin(&self) -> Result<Option<Recovery>> {
        self.ensure_ready()?;
        let mut state = self.state.lock().unwrap();
        if !state.enabled {
            return Ok(None);
        }
        let id = next_id()?;
        let (reply, received) = oneshot::channel();
        state.pending = Some(Pending {
            id,
            deadline: None,
            reply: Some(reply),
        });
        Ok(Some(Recovery {
            owner: self.clone(),
            id,
            received: Some(received),
            finished: false,
        }))
    }
    pub fn acknowledge(&self, acknowledgement: Acknowledgement) -> Result<()> {
        ensure!(
            acknowledgement.id > 0,
            invalid("Invalid USB peripheral recovery ID")
        );
        if let Some(error) = &acknowledgement.error {
            ensure!(
                error.len() <= 2048 && !error.chars().any(|c| c.is_control()),
                invalid("Invalid USB peripheral recovery error")
            );
            ensure!(
                !acknowledgement.success,
                invalid("Successful USB peripheral recovery cannot include an error")
            );
        }
        let mut state = self.state.lock().unwrap();
        let pending = state
            .pending
            .as_mut()
            .ok_or_else(|| invalid("No USB peripheral recovery acknowledgement is pending"))?;
        ensure!(
            pending.id == acknowledgement.id,
            invalid("USB peripheral recovery acknowledgement does not match the pending episode")
        );
        ensure!(
            pending
                .deadline
                .is_some_and(|deadline| Instant::now() < deadline),
            invalid("USB peripheral recovery is not awaiting acknowledgement or has expired")
        );
        let reply = pending
            .reply
            .take()
            .ok_or_else(|| invalid("USB peripheral recovery was already acknowledged"))?;
        reply
            .send(acknowledgement)
            .map_err(|_| invalid("USB peripheral recovery is no longer waiting"))?;
        Ok(())
    }
    pub(crate) fn retire(&self) {
        let mut state = self.state.lock().unwrap();
        state.retired = true;
        state.pending = None;
        self.status.lock().unwrap().usb_peripheral_recovery_id = None;
    }
}

pub(crate) struct Recovery {
    owner: PeripheralRecoveryHandle,
    pub(crate) id: u64,
    received: Option<oneshot::Receiver<Acknowledgement>>,
    finished: bool,
}
impl Recovery {
    pub(crate) async fn wait(mut self, token: &CancellationToken) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(WAIT_SECONDS);
        {
            let mut state = self.owner.state.lock().unwrap();
            let pending = state
                .pending
                .as_mut()
                .filter(|p| p.id == self.id)
                .ok_or_else(|| invalid("USB peripheral recovery was retired"))?;
            pending.deadline = Some(deadline);
            self.owner.status.lock().unwrap().usb_peripheral_recovery_id = Some(self.id);
        }
        let result = tokio::select! { biased;
            _ = token.cancelled() => Err(Failure::Cancelled.into()),
            _ = tokio::time::sleep_until(deadline) => Err(invalid("NINA peripheral recovery confirmation timed out")),
            reply = self.received.take().unwrap() => match reply {
                Ok(reply) if reply.success => Ok(()),
                Ok(reply) => Err(invalid(format!("NINA peripheral recovery failed: {}",
                    reply.error.as_deref().unwrap_or("peripherals were not confirmed ready")))),
                Err(_) => Err(invalid("USB peripheral recovery was retired")),
            }
        };
        self.finish(result.as_ref().err().map(|error| format!("{error:#}")));
        result
    }
    fn finish(&mut self, failure: Option<String>) {
        let mut state = self.owner.state.lock().unwrap();
        if state.pending.as_ref().is_some_and(|p| p.id == self.id) {
            state.pending = None;
            state.blocked = failure.map(|message| format!(
                "USB peripheral recovery was not confirmed ({message}); disconnect and reconnect the camera before another exposure"));
            self.owner.status.lock().unwrap().usb_peripheral_recovery_id = None;
        }
        self.finished = true;
    }
}
impl Drop for Recovery {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(Some("recovery did not complete".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Status;
    fn handle() -> PeripheralRecoveryHandle {
        let handle = PeripheralRecoveryHandle::new(Arc::new(Mutex::new(Status::default())));
        handle.enable(true).unwrap();
        handle
    }
    fn ack(id: u64, success: bool) -> Acknowledgement {
        Acknowledgement {
            id,
            success,
            error: None,
        }
    }
    async fn waiting(
        handle: &PeripheralRecoveryHandle,
        token: CancellationToken,
    ) -> (u64, tokio::task::JoinHandle<Result<()>>) {
        let recovery = handle.begin().unwrap().unwrap();
        let id = recovery.id;
        assert!(
            handle
                .status
                .lock()
                .unwrap()
                .usb_peripheral_recovery_id
                .is_none()
        );
        assert!(
            handle.acknowledge(ack(id, true)).is_err(),
            "no ACK before camera return"
        );
        let wait = tokio::spawn(async move { recovery.wait(&token).await });
        tokio::task::yield_now().await;
        assert_eq!(
            handle.status.lock().unwrap().usb_peripheral_recovery_id,
            Some(id)
        );
        (id, wait)
    }
    #[tokio::test(start_paused = true)]
    async fn replacement_waits_for_matching_success_and_repeated_old_future_ack_is_rejected() {
        let handle = handle();
        let (id, wait) = waiting(&handle, CancellationToken::new()).await;
        tokio::time::advance(Duration::from_secs(119)).await;
        assert!(
            !wait.is_finished(),
            "replacement must not start before confirmation"
        );
        assert!(handle.ensure_ready().is_err());
        for wrong in [0, id - 1, id + 1] {
            assert!(handle.acknowledge(ack(wrong, true)).is_err());
        }
        handle.acknowledge(ack(id, true)).unwrap();
        assert!(handle.acknowledge(ack(id, true)).is_err());
        wait.await.unwrap().unwrap();
        handle.ensure_ready().unwrap();
        assert!(
            handle
                .status
                .lock()
                .unwrap()
                .usb_peripheral_recovery_id
                .is_none()
        );
        let (next, wait) = waiting(&handle, CancellationToken::new()).await;
        assert!(next > id);
        assert!(handle.acknowledge(ack(id, true)).is_err());
        handle.acknowledge(ack(next, true)).unwrap();
        wait.await.unwrap().unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn failed_ack_is_accepted_but_blocks_this_and_future_exposures() {
        let handle = handle();
        let (id, wait) = waiting(&handle, CancellationToken::new()).await;
        handle
            .acknowledge(Acknowledgement {
                id,
                success: false,
                error: Some("Filter wheel did not reconnect".into()),
            })
            .unwrap();
        let error = wait.await.unwrap().unwrap_err();
        assert!(!crate::retryable(&error));
        assert!(error.to_string().contains("Filter wheel"));
        assert!(handle.ensure_ready().is_err());
        assert!(handle.begin().is_err());
        assert!(handle.acknowledge(ack(id, true)).is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn expiry_and_cancel_fence_future_starts_and_clear_pending_status() {
        for cancel in [false, true] {
            let handle = handle();
            let token = CancellationToken::new();
            let (id, wait) = waiting(&handle, token.clone()).await;
            if cancel {
                token.cancel();
            } else {
                tokio::time::advance(Duration::from_secs(WAIT_SECONDS)).await;
            }
            assert!(wait.await.unwrap().is_err());
            assert!(
                handle
                    .status
                    .lock()
                    .unwrap()
                    .usb_peripheral_recovery_id
                    .is_none()
            );
            assert!(handle.ensure_ready().is_err());
            assert!(handle.acknowledge(ack(id, true)).is_err());
        }
    }
    #[tokio::test(start_paused = true)]
    async fn cancelled_confirmation_wins_over_queued_success_and_retirement_rejects_ack() {
        let first = handle();
        let token = CancellationToken::new();
        let (id, wait) = waiting(&first, token.clone()).await;
        first.acknowledge(ack(id, true)).unwrap();
        token.cancel();
        assert!(wait.await.unwrap().unwrap_err().is::<Failure>());
        assert!(first.ensure_ready().is_err());
        let fresh = handle();
        let (next, wait) = waiting(&fresh, CancellationToken::new()).await;
        assert!(next > id, "new Session must not reuse episode IDs");
        fresh.retire();
        assert!(wait.await.unwrap().is_err());
        assert!(fresh.acknowledge(ack(next, true)).is_err());
    }
    #[test]
    fn default_is_disabled_and_incomplete_reset_is_a_safety_fence() {
        let status = Arc::new(Mutex::new(Status::default()));
        let handle = PeripheralRecoveryHandle::new(status);
        assert!(handle.begin().unwrap().is_none());
        handle.enable(true).unwrap();
        drop(handle.begin().unwrap().unwrap());
        assert!(handle.ensure_ready().is_err());
        assert!(handle.enable(false).is_err());
        assert!(
            handle.reconnect_after_close().is_err(),
            "connect alone must not clear a failed episode"
        );
        handle.retire();
        handle.reconnect_after_close().unwrap();
        assert!(
            handle.begin().unwrap().is_some(),
            "explicit reconnect preserves opt-in"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn malformed_or_conflicting_ack_cannot_consume_a_valid_pending_reply() {
        for value in [
            serde_json::json!({"id":1,"success":"true"}),
            serde_json::json!({"id":-1,"success":true}),
            serde_json::json!({"id":1,"success":true,"other":true}),
        ] {
            assert!(serde_json::from_value::<Acknowledgement>(value).is_err());
        }
        let handle = handle();
        let (id, wait) = waiting(&handle, CancellationToken::new()).await;
        for error in [
            "line\nbreak".into(),
            "x".repeat(2049),
            "unexpected success error".into(),
        ] {
            assert!(
                handle
                    .acknowledge(Acknowledgement {
                        id,
                        success: true,
                        error: Some(error)
                    })
                    .is_err()
            );
        }
        handle.acknowledge(ack(id, true)).unwrap();
        wait.await.unwrap().unwrap();
    }
}
