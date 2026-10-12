//! Live camera controls shared by all frontends, serviced at worker checkpoints.
use crate::RecoveryOptions;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CameraRuntimeOptions {
    pub transfer_log_mode: String,
    pub cooler_max_power_change_per_5_seconds: f64,
}
impl CameraRuntimeOptions {
    pub fn from_recovery(options: &RecoveryOptions) -> Self {
        Self {
            transfer_log_mode: options.transfer_log_mode.clone(),
            cooler_max_power_change_per_5_seconds: options.cooler_max_power_change_per_5_seconds,
        }
    }
    pub fn validate(&self) -> Result<()> {
        let mut recovery = RecoveryOptions::default();
        self.apply(&mut recovery);
        recovery.validate()
    }
    pub fn apply(&self, recovery: &mut RecoveryOptions) {
        recovery
            .transfer_log_mode
            .clone_from(&self.transfer_log_mode);
        recovery.cooler_max_power_change_per_5_seconds = self.cooler_max_power_change_per_5_seconds;
    }
}
#[derive(Clone)]
pub struct CameraRuntimeHandle(Arc<Mutex<CameraRuntimeOptions>>);
impl CameraRuntimeHandle {
    pub(crate) fn new(options: &RecoveryOptions) -> Self {
        Self(Arc::new(Mutex::new(CameraRuntimeOptions::from_recovery(
            options,
        ))))
    }
    pub fn get(&self) -> CameraRuntimeOptions {
        self.0.lock().unwrap().clone()
    }
    pub fn update(&self, options: CameraRuntimeOptions) -> Result<()> {
        options.validate()?;
        *self.0.lock().unwrap() = options;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_controls_are_shared_with_one_owner_and_invalid_changes_are_inert() {
        let recovery = RecoveryOptions::default();
        let first = CameraRuntimeHandle::new(&recovery);
        let owner = first.clone();
        let second = CameraRuntimeHandle::new(&recovery);
        let mut options = first.get();
        options.transfer_log_mode = "off".into();
        options.cooler_max_power_change_per_5_seconds = 5.0;
        first.update(options.clone()).unwrap();
        assert_eq!(owner.get(), options);
        assert_eq!(second.get(), CameraRuntimeOptions::from_recovery(&recovery));
        for invalid in [0.0, f64::NAN, 101.0] {
            let mut bad = options.clone();
            bad.cooler_max_power_change_per_5_seconds = invalid;
            assert!(first.update(bad).is_err());
            assert_eq!(owner.get(), options);
        }
        let mut bad = options.clone();
        bad.transfer_log_mode = "anything".into();
        assert!(first.update(bad).is_err());
        assert_eq!(owner.get(), options);
    }
}
