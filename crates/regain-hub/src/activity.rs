//! Retained runtime work prevents configuration replacement after client loss.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;

#[derive(Default)]
struct State {
    active: AtomicUsize,
    changed: Notify,
}

#[derive(Clone, Default)]
pub struct ActivityCounter(Arc<State>);
impl ActivityCounter {
    pub(crate) fn shares(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn active(&self) -> usize {
        self.0.active.load(Ordering::SeqCst)
    }
    /// A retirement barrier only after the owner has stopped admitting new work.
    /// Waiting does not cancel tasks or turn a timeout into completed cleanup.
    pub(crate) async fn wait_idle(&self) {
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.active() == 0 {
                return;
            }
            notified.await;
        }
    }
}
pub(crate) struct Activity(ActivityCounter);
impl Activity {
    pub(crate) fn new(counter: ActivityCounter) -> Self {
        counter.0.active.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        if self.0.0.active.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.0.changed.notify_waiters();
        }
    }
}
