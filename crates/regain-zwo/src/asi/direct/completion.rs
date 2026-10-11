//! A cleanup failure does not invalidate pixels already in host memory.
use anyhow::Result;
use serde_json::Value;
/// Only a successfully stopped acquisition can acknowledge cooperative abort.
#[derive(Debug)]
pub(super) struct CaptureCancelled;
impl std::fmt::Display for CaptureCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Capture cancelled; camera stopped")
    }
}
impl std::error::Error for CaptureCancelled {}

/// Always attempt the model's ordinary full stop after clock restoration,
/// including when restoration failed. Clearing retained DDR does not prove the
/// clocks were restored, so preserve either failure and combine both if needed.
pub(super) fn finish_stop(
    model: &str,
    wake: Result<()>,
    stop: impl FnOnce() -> Result<()>,
) -> Result<()> {
    match (wake, stop()) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(wake), Err(stop)) => {
            anyhow::bail!("{model} clock wake failed: {wake:#}; full stop failed: {stop:#}")
        }
    }
}

pub fn finish(result: Result<(Value, Vec<u8>)>, cleanup: Result<()>) -> Result<(Value, Vec<u8>)> {
    if result
        .as_ref()
        .err()
        .is_some_and(|e| e.is::<CaptureCancelled>())
        && let Err(error) = &cleanup
    {
        anyhow::bail!("Capture cancelled but camera stop failed: {error:#}");
    }
    if let Err(error) = &cleanup {
        crate::asi::direct::diagnostics::log(
            "warning",
            "capture.cleanup_failed",
            format_args!(
                "{error:#}; {}",
                if result.is_ok() {
                    "completed frame preserved"
                } else {
                    "capture also failed"
                }
            ),
        );
    }
    let (mut metadata, pixels) = result?;
    // Normalize legacy ASI676 metadata at the common completion boundary.
    if metadata.get("readRecoveries").is_none()
        && let Some(reads) = metadata.get("readoutRetriesUsed").cloned()
    {
        metadata["readRecoveries"] = reads;
    }
    if let Err(error) = cleanup {
        metadata["cleanupError"] = Value::String(format!("{error:#}"));
    }
    let reads = metadata["readRecoveries"].as_u64().unwrap_or(0);
    let discarded = metadata["discardedStartupFrames"].as_u64().unwrap_or(0);
    if reads > 0 || discarded > 0 {
        crate::asi::direct::diagnostics::log(
            "info",
            "capture.recovered",
            format_args!(
                "Frame ready after {reads} same-frame read retries and {discarded} discarded guide-stream frames"
            ),
        );
    }
    Ok((metadata, pixels))
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn full_stop_is_attempted_after_wake_failure_for_every_clocked_model() {
        for model in ["ASI2600", "ASI6200", "ASI585", "ASI662", "ASI676"] {
            for stop_fails in [false, true] {
                let stopped = Cell::new(false);
                let cleanup = finish_stop(model, Err(anyhow::anyhow!("gate write failed")), || {
                    stopped.set(true);
                    if stop_fails {
                        anyhow::bail!("AA failed");
                    }
                    Ok(())
                });
                assert!(stopped.get(), "{model} must attempt full stop");
                let error = finish(Err(CaptureCancelled.into()), cleanup).unwrap_err();
                assert!(!error.is::<CaptureCancelled>());
                assert!(error.to_string().contains("gate write failed"));
                if stop_fails {
                    assert!(error.to_string().contains(model));
                    assert!(error.to_string().contains("AA failed"));
                }
            }
        }
    }

    #[test]
    fn clean_wake_does_not_hide_full_stop_failure() {
        let stopped = Cell::new(false);
        let error = finish_stop("ASI6200", Ok(()), || {
            stopped.set(true);
            anyhow::bail!("pipe reset failed")
        })
        .unwrap_err();
        assert!(stopped.get());
        assert_eq!(error.to_string(), "pipe reset failed");
        finish_stop("ASI6200", Ok(()), || Ok(())).unwrap();
    }

    #[test]
    fn cancellation_only_acknowledges_successful_cleanup() {
        assert!(
            finish(Err(CaptureCancelled.into()), Ok(()))
                .unwrap_err()
                .is::<CaptureCancelled>()
        );
        let error = finish(
            Err(CaptureCancelled.into()),
            Err(anyhow::anyhow!("stop failed")),
        )
        .unwrap_err();
        assert!(!error.is::<CaptureCancelled>());
        assert!(error.to_string().contains("stop failed"));
    }
    #[test]
    fn preserves_valid_pixels_and_original_acquisition_errors() {
        let (meta, pixels) = finish(
            Ok((serde_json::json!({"width":2}), vec![1, 2, 3, 4])),
            Err(anyhow::anyhow!("reset failed")),
        )
        .unwrap();
        assert_eq!(pixels, [1, 2, 3, 4]);
        assert_eq!(meta["cleanupError"], "reset failed");
        assert_eq!(
            finish(
                Err(anyhow::anyhow!("bad frame")),
                Err(anyhow::anyhow!("stop failed"))
            )
            .unwrap_err()
            .to_string(),
            "bad frame"
        );
    }
    #[test]
    fn asi676_legacy_counts_reach_the_common_protocol() {
        let (metadata, _) = finish(
            Ok((serde_json::json!({"readoutRetriesUsed":2}), vec![])),
            Ok(()),
        )
        .unwrap();
        assert_eq!(metadata["readRecoveries"], 2);
    }
}
