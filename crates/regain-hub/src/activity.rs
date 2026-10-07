//! Retained runtime work prevents configuration replacement after client loss.
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Default)]
pub struct ActivityCounter(Arc<AtomicUsize>);
impl ActivityCounter {
    pub fn active(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}
pub(crate) struct Activity(ActivityCounter);
impl Activity {
    pub(crate) fn new(counter: ActivityCounter) -> Self {
        counter.0.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::SeqCst);
    }
}
