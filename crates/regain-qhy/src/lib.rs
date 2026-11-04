//! QHY camera protocols. Vendor API numbers and sequencing stay in this crate.
mod cfw;
pub mod cfw_usb;
mod diagnostics;
#[cfg(windows)]
pub mod direct;
#[cfg(any(windows, test))]
mod framing;
pub mod sdk;
mod worker;
