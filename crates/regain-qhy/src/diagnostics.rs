//! Both QHY backends use the common asynchronous recorder and runtime toggle.
pub use regain_core::diagnostics::configure;
use serde_json::{Value, json};
pub fn record(details: Value) {
    let operation = details
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or("QHY transfer")
        .to_owned();
    regain_core::diagnostics::queued_details("debug", "usb.qhy", operation, details);
}
pub fn sdk(details: Value) {
    let operation = details["operation"]
        .as_str()
        .unwrap_or("QHY SDK")
        .to_owned();
    regain_core::diagnostics::queued_details("debug", "sdk.api.qhy", operation, details);
}
pub fn dump(error: &str) {
    regain_core::diagnostics::queued_details(
        "warning",
        "command.failed",
        error,
        json!({"vendor":"qhy"}),
    );
    regain_core::diagnostics::flush_usb();
}
