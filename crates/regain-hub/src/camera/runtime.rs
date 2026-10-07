//! Host-owned camera resources outlive applied configuration revisions and
//! disconnected readers. Construct once in the host's runtime-builder closure.
use super::image::{ImageBudget, MAX_IMAGE_BYTES};
use crate::{activity::ActivityCounter, source::SourceError};
use regain_core::Diagnostic;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone)]
pub struct CameraResources {
    budget: ImageBudget,
    activity: ActivityCounter,
}
impl CameraResources {
    pub(crate) fn from_parts(budget: ImageBudget, activity: ActivityCounter) -> Self {
        Self { budget, activity }
    }
    pub(crate) fn shares(&self, other: &Self) -> bool {
        self.budget.shares(&other.budget) && self.activity.shares(&other.activity)
    }
    /// Admission does not allocate an image or access equipment. Clones retain
    /// exactly the same accounting, including images pinned by retired clients.
    pub fn new(maximum_image_bytes: usize) -> Result<Self, SourceError> {
        Ok(Self {
            budget: ImageBudget::new(maximum_image_bytes)?,
            activity: ActivityCounter::default(),
        })
    }
    pub fn image_budget(&self) -> ImageBudget {
        self.budget.clone()
    }
    pub fn activity(&self) -> ActivityCounter {
        self.activity.clone()
    }
}
impl Default for CameraResources {
    fn default() -> Self {
        Self::new(MAX_IMAGE_BYTES).expect("Default camera budget is positive")
    }
}

/// Host-selected native camera settings; the source supplies model/serial and
/// recovery. SDK simulation is explicit, never inferred from an I/O failure.
#[derive(Clone)]
pub struct NativeCameraRuntime {
    pub sdk: PathBuf,
    pub sdk_simulation: Option<Value>,
    pub resources: CameraResources,
    pub diagnostic: Diagnostic,
}
