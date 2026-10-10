//! ZWO ASI camera backends. Each runs in an isolated regain-device process.
mod continuous;
#[cfg(feature = "asi-direct")]
pub mod direct;
#[cfg(feature = "asi-sdk")]
pub mod sdk;

/// Explicit disconnect shuts down only advertised thermal actuators. Try both
/// even if one fails, and require readback before claiming it is disabled.
fn shutdown_thermal_controls(
    controls: &[serde_json::Value],
    mut disable: impl FnMut(i32) -> anyhow::Result<i64>,
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for kind in [17, 21] {
        if !controls.iter().any(|c| {
            c["type"] == kind
                && c["writable"] == true
                && c["min"].as_i64().is_some_and(|v| v <= 0)
                && c["max"].as_i64().is_some_and(|v| v >= 0)
        }) {
            continue;
        }
        match disable(kind) {
            Ok(0) => eprintln!(
                "REGAIN_DIAGNOSTIC {}",
                serde_json::json!({
                    "version":1,"level":"info","event":"camera.thermal_disabled",
                    "pid":std::process::id(),"message":format!("Control {kind} disabled on disconnect")
                })
            ),
            Ok(value) => failures.push(format!("thermal control {kind} off readback was {value}")),
            Err(error) => failures.push(format!("disable thermal control {kind}: {error:#}")),
        }
    }
    anyhow::ensure!(failures.is_empty(), "{}", failures.join("; "));
    Ok(())
}

/// Empty user input is an unspecified identity, never an empty serial filter.
pub(crate) fn normalized_serial(serial: Option<&str>) -> Option<String> {
    serial
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
}

#[cfg(test)]
mod tests {
    use super::shutdown_thermal_controls;
    use serde_json::json;

    #[test]
    fn thermal_shutdown_respects_capabilities_and_attempts_both_after_failure() {
        let caps = [17, 21, 0].map(|kind| json!({"type":kind,"writable":true,"min":0,"max":100}));
        let mut attempted = Vec::new();
        let error = shutdown_thermal_controls(&caps, |kind| {
            attempted.push(kind);
            if kind == 17 {
                anyhow::bail!("cooler disconnected");
            }
            Ok(0)
        })
        .unwrap_err();
        assert_eq!(attempted, [17, 21]);
        assert!(error.to_string().contains("cooler disconnected"));
        assert!(
            shutdown_thermal_controls(&caps, |_| Ok(1))
                .unwrap_err()
                .to_string()
                .contains("readback")
        );
        let unsupported = [
            json!({"type":17,"writable":false,"min":0,"max":1}),
            json!({"type":21,"writable":true,"min":1,"max":1}),
        ];
        shutdown_thermal_controls(&unsupported, |_| panic!("unsupported actuator")).unwrap();
        shutdown_thermal_controls(&caps, |_| Ok(0)).unwrap();
    }
    #[test]
    fn serial_input_is_optional_and_case_insensitive() {
        for input in [None, Some(""), Some("  ")] {
            assert_eq!(super::normalized_serial(input), None);
        }
        assert_eq!(
            super::normalized_serial(Some(" ABCDef0123456789 ")).as_deref(),
            Some("abcdef0123456789")
        );
    }
}
