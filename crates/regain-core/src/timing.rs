//! Outer supervision allowances for the existing native recovery workflow.
//! These do not change command deadlines, retry eligibility or recovery policy.
//! OS process startup/retirement and executor scheduling have no strict wall-time
//! bound: an expired caller must still retain and drain owned worker cleanup.
use crate::{RecoveryOptions, Selection, invalid};
use anyhow::{Result, ensure};
use std::time::Duration;

pub(crate) const PERSISTENT_CONTROLS: [i32; 18] = [
    0, 2, 3, 4, 5, 6, 7, 9, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23,
];
pub(crate) const ACKNOWLEDGED_CONTROLS: [i32; 4] = [0, 5, 16, 17];
pub(crate) const SIMULATION_SETUP_SECONDS: f64 = 5.;
pub(crate) const USB_BIND_SECONDS: u64 = 30;
pub(crate) const USB_RESET_SECONDS: u64 = 80;
pub(crate) const USB_REBIND_PAUSE_SECONDS: f64 = 0.5;
pub(crate) const CLOSE_SECONDS: f64 = 2.;
pub(crate) const MAX_READ_RETRY_OVERHEAD_SECONDS: f64 = 15.;
const SUPERVISOR_MARGIN_SECONDS: f64 = 5.;

#[derive(Clone, Debug)]
pub struct NativeCameraTiming {
    options: RecoveryOptions,
    direct: bool,
    sdk_fallback: bool,
}
impl NativeCameraTiming {
    /// No probing or equipment I/O. The selection must remain fixed for this
    /// owner; dynamic SDK fallback is already covered by the original selection.
    pub fn new(selection: &Selection) -> Result<Self> {
        selection.recovery.validate()?;
        Ok(Self {
            options: selection.recovery.clone(),
            direct: selection.direct,
            sdk_fallback: selection.direct && selection.sdk_fallback,
        })
    }
    fn command(&self) -> f64 {
        self.options.command_timeout_seconds
    }
    fn open_seconds(&self) -> f64 {
        // Open plus up to two software-white-balance restoration commands.
        // Include explicit SDK fixture setup without depending on simulation.
        SIMULATION_SETUP_SECONDS + 3. * self.command()
    }
    fn restore_seconds(&self) -> f64 {
        // Acknowledged controls share one deadline for write/readback; each
        // other persistent control can require separate set and get commands.
        (2 * PERSISTENT_CONTROLS.len() - ACKNOWLEDGED_CONTROLS.len()) as f64 * self.command()
    }
    fn close_seconds(&self) -> f64 {
        CLOSE_SECONDS
            + if self.direct {
                // Failed direct close can reopen the exact cooled camera to
                // disable its cooler, then close again. SDK fallback only
                // reduces this allowance; the original direct choice is safe.
                self.options.reconnect_delay_seconds
                    + self.open_seconds()
                    + self.command()
                    + CLOSE_SECONDS
            } else {
                0.
            }
    }
    fn usb_reset_seconds(&self) -> f64 {
        // Rebinding can begin just before the reappearance deadline and spend
        // one full bind-command allowance beyond it, plus its preceding delay.
        USB_RESET_SECONDS as f64 + 2. * USB_BIND_SECONDS as f64 + USB_REBIND_PAUSE_SECONDS
    }
    fn cleanup_seconds(&self) -> f64 {
        // A connecting task can close before opening, on failure and after a
        // generation fence; the final source close also needs its own cleanup.
        // A dispatched USB reset finishes before observing cancellation.
        4. * self.close_seconds()
            + self.command()
            + if self.options.usb_reset_after_failures > 0 {
                USB_RESET_SECONDS as f64
            } else {
                0.
            }
            + SUPERVISOR_MARGIN_SECONDS
    }
    pub fn cleanup_allowance(&self) -> Duration {
        duration(self.cleanup_seconds()).expect("validated native cleanup timing")
    }
    pub fn connection_allowance(&self) -> Duration {
        let usb = self.options.usb_reset_after_failures > 0;
        let opens = 1 + usize::from(self.sdk_fallback) + usize::from(usb);
        // Include old-generation drain, this connection's three possible closes,
        // direct->SDK open fallback, USB binding/reopen and initial control/env
        // restoration performed by the native owner before it publishes ready.
        duration(
            self.cleanup_seconds()
                + 3. * self.close_seconds()
                + opens as f64 * self.open_seconds()
                + if self.sdk_fallback {
                    self.options.reconnect_delay_seconds
                } else {
                    0.
                }
                + if usb { USB_BIND_SECONDS as f64 } else { 0. }
                + self.restore_seconds()
                + 2. * self.command()
                + SUPERVISOR_MARGIN_SECONDS,
        )
        .expect("validated native connection timing")
    }
    pub fn control_allowance(&self) -> Duration {
        // A setting may await idle telemetry (two reads), restore an Abort-
        // retired worker and all known controls/environment, then acknowledge
        // the new value. Ordinary polling still retains its scalar deadline.
        duration(
            self.options.reconnect_delay_seconds
                + self.open_seconds()
                + self.restore_seconds()
                + 5. * self.command()
                + SUPERVISOR_MARGIN_SECONDS,
        )
        .expect("validated native control timing")
    }
    pub fn capture_allowance(&self, microseconds: u64) -> Result<Duration> {
        ensure!(
            microseconds > 0,
            invalid("Invalid native exposure duration")
        );
        let o = &self.options;
        let replacements = o.replacement_exposures(microseconds);
        let settle = o.cooling_timeout_seconds + 3. * self.command() + o.cooling_sample_seconds;
        let ready = microseconds as f64 / 1e6
            + o.exposure_grace_seconds
            + if self.direct {
                // Device capability is learned later. Conservatively cover
                // retained direct rereads even above the replacement limit.
                (1 + o.direct_read_retries) as f64 * o.download_timeout_seconds
                    + o.direct_read_retries as f64 * MAX_READ_RETRY_OVERHEAD_SECONDS
            } else {
                0.
            };
        let reads = 1 + o.ready_frame_download_retries;
        let download = reads as f64 * (o.download_timeout_seconds + self.command())
            + o.ready_frame_download_retries as f64 * o.reconnect_delay_seconds;
        let fallback = if self.sdk_fallback {
            // Validate can reject direct capture after its first restoration
            // and settle, requiring SDK open/restoration/settling again.
            self.command()
                + o.reconnect_delay_seconds
                + self.open_seconds()
                + self.restore_seconds()
                + settle
        } else {
            0.
        };
        let attempt = o.reconnect_delay_seconds
            + self.open_seconds()
            + self.restore_seconds()
            + settle
            + fallback
            + ready
            + download
            // Service checkpoints, two environment reads, Start, readiness
            // overshoot (environment/service/status), and final cooler service.
            + 10. * self.command()
            + 0.025;
        duration(
            (1 + replacements) as f64 * attempt
                + if o.usb_reset_after_failures > 0 && o.usb_reset_after_failures <= replacements {
                    self.usb_reset_seconds()
                } else {
                    0.
                }
                + SUPERVISOR_MARGIN_SECONDS,
        )
    }
}
fn duration(seconds: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(seconds).map_err(|_| invalid("Native timing is not representable"))
}

impl RecoveryOptions {
    /// Same microsecond exposure and inclusive threshold used by core capture
    /// and every native outer supervisor. Rereads have separate eligibility.
    pub fn replacement_exposures(&self, microseconds: u64) -> u32 {
        if microseconds as f64 / 1e6 <= self.maximum_retry_exposure_seconds {
            self.max_retries
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selection(direct: bool) -> Selection {
        Selection {
            name: "Timing-only fixture".into(),
            serial: None,
            direct,
            sdk_fallback: false,
            recovery: RecoveryOptions::default(),
        }
    }
    #[test]
    fn replacement_threshold_is_inclusive_and_long_captures_get_no_replacements() {
        let mut selection = selection(true);
        assert_eq!(selection.recovery.replacement_exposures(30_000_000), 3);
        assert_eq!(selection.recovery.replacement_exposures(30_000_001), 0);
        let timing = NativeCameraTiming::new(&selection).unwrap();
        assert!(
            timing.capture_allowance(30_000_000).unwrap()
                > timing.capture_allowance(30_000_001).unwrap()
        );
        selection.recovery.max_retries = 20;
        assert_eq!(
            NativeCameraTiming::new(&selection)
                .unwrap()
                .capture_allowance(600_000_000)
                .unwrap(),
            timing.capture_allowance(600_000_000).unwrap()
        );
        selection.recovery.direct_read_retries = 5;
        assert!(
            NativeCameraTiming::new(&selection)
                .unwrap()
                .capture_allowance(600_000_000)
                .unwrap()
                > timing.capture_allowance(600_000_000).unwrap(),
            "retained rereads outlive the replacement threshold"
        );
    }
    #[test]
    fn usb_capture_allowance_requires_a_reachable_replacement_and_counts_one_reset() {
        let mut selection = selection(true);
        let base = NativeCameraTiming::new(&selection).unwrap();
        selection.recovery.usb_reset_after_failures = 4;
        assert_eq!(
            NativeCameraTiming::new(&selection)
                .unwrap()
                .capture_allowance(1_000_000)
                .unwrap(),
            base.capture_allowance(1_000_000).unwrap()
        );
        selection.recovery.usb_reset_after_failures = 1;
        let reset = NativeCameraTiming::new(&selection).unwrap();
        assert_eq!(
            reset.capture_allowance(1_000_000).unwrap()
                - base.capture_allowance(1_000_000).unwrap(),
            Duration::from_millis(140_500)
        );
        assert_eq!(
            reset.capture_allowance(600_000_000).unwrap(),
            base.capture_allowance(600_000_000).unwrap()
        );
    }
    #[test]
    fn sdk_fallback_and_late_control_restoration_are_included_only_for_native_paths() {
        let mut selection = selection(true);
        let direct = NativeCameraTiming::new(&selection).unwrap();
        selection.sdk_fallback = true;
        let fallback = NativeCameraTiming::new(&selection).unwrap();
        assert!(fallback.connection_allowance() > direct.connection_allowance());
        assert!(
            fallback.capture_allowance(1_000_000).unwrap()
                > direct.capture_allowance(1_000_000).unwrap()
        );
        selection.direct = false;
        let sdk = NativeCameraTiming::new(&selection).unwrap();
        selection.sdk_fallback = false;
        let no_fallback = NativeCameraTiming::new(&selection).unwrap();
        assert_eq!(
            sdk.connection_allowance(),
            no_fallback.connection_allowance()
        );
        assert_eq!(
            sdk.capture_allowance(1_000_000).unwrap(),
            no_fallback.capture_allowance(1_000_000).unwrap()
        );
        assert!(sdk.control_allowance() > Duration::from_secs(60));
        assert!(sdk.connection_allowance() > Duration::from_secs(300));
        assert!(sdk.capture_allowance(1_000_000).unwrap() > Duration::from_secs(300));
    }
    #[test]
    fn all_accepted_policy_maxima_produce_representable_positive_allowances() {
        use crate::recovery::RecoveryType;
        let mut values = serde_json::Map::new();
        for field in RecoveryOptions::fields() {
            values.insert(
                field.key.into(),
                match field.value_type {
                    RecoveryType::Integer(_, max) | RecoveryType::PowerOfTwo(_, max) => {
                        serde_json::json!(max)
                    }
                    RecoveryType::Number { maximum, .. } => serde_json::json!(maximum),
                    RecoveryType::Boolean => serde_json::json!(true),
                },
            );
        }
        for direct in [false, true] {
            let mut selection = selection(direct);
            selection.recovery = serde_json::from_value(values.clone().into()).unwrap();
            selection.sdk_fallback = true;
            let timing = NativeCameraTiming::new(&selection).unwrap();
            for duration in [
                timing.connection_allowance(),
                timing.control_allowance(),
                timing.cleanup_allowance(),
                timing.capture_allowance(86_400_000_000).unwrap(),
            ] {
                assert!(!duration.is_zero());
                assert!(std::time::Instant::now().checked_add(duration).is_some());
            }
            assert!(timing.capture_allowance(u64::MAX).is_ok());
            assert!(timing.capture_allowance(0).is_err());
        }
    }
    #[test]
    fn invalid_recovery_policy_never_reaches_duration_conversion() {
        let mut selection = selection(false);
        selection.recovery.command_timeout_seconds = f64::NAN;
        assert!(NativeCameraTiming::new(&selection).is_err());
        selection.recovery.command_timeout_seconds = 3600.01;
        assert!(NativeCameraTiming::new(&selection).is_err());
    }
}
