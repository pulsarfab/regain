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
