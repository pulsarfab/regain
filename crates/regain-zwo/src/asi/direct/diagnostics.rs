//! Direct-driver diagnostics share the SDK transport's bounded stderr writer.
pub(super) use crate::asi::diagnostics::{UsbTrace, control_fields, control_payload, flush_usb};
pub use crate::asi::diagnostics::{details, log};
use std::fmt::Display;

pub fn read_failure(camera: &str, error: impl Display, used: usize, limit: u32, retained: bool) {
    let retry = used < limit as usize;
    let action = if retry {
        format!(
            "retry {}/{limit}: {}",
            used + 1,
            if retained {
                "rereading the same frame from byte zero"
            } else {
                "resynchronizing the guide stream with a subsequent frame"
            }
        )
    } else {
        "read retry limit reached".into()
    };
    log(
        "warning",
        if retry {
            "transfer.retry"
        } else {
            "transfer.exhausted"
        },
        format_args!("{camera}: {action}; {error}"),
    );
}
