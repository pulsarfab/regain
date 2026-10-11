//! Preserve acknowledged environment state across sensor initialization.
use anyhow::Result;

/// Initialization can reset thermal hardware without explicit thermal writes.
/// Always reapply after the entire model-specific configuration/calibration
/// sequence, including its error paths, before another exposure can be armed.
pub(super) fn preserve_environment<T>(
    model: &str,
    initialize: impl FnOnce() -> Result<T>,
    restore: impl FnOnce() -> Result<()>,
) -> Result<T> {
    let initialized = initialize();
    match (initialized, restore()) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(restore)) => anyhow::bail!(
            "{model} initialization failed: {error:#}; thermal restoration failed: {restore:#}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asi::direct::completion::CaptureCancelled;
    use std::cell::Cell;

    #[test]
    fn initialization_restores_after_failure_and_rejects_uncertain_cancellation() {
        for model in ["ASI585", "ASI2600", "ASI6200"] {
            let hardware_heater = Cell::new(true);
            let error = preserve_environment(
                model,
                || {
                    hardware_heater.set(false);
                    Err::<(), _>(CaptureCancelled.into())
                },
                || {
                    hardware_heater.set(true);
                    Ok(())
                },
            )
            .unwrap_err();
            assert!(hardware_heater.get());
            assert!(error.is::<CaptureCancelled>());

            let error = preserve_environment(
                model,
                || Err::<(), _>(CaptureCancelled.into()),
                || anyhow::bail!("heater restore failed"),
            )
            .unwrap_err();
            assert!(!error.is::<CaptureCancelled>());
            assert!(error.to_string().starts_with(model));
            assert!(error.to_string().contains("heater restore failed"));
        }
    }
}
