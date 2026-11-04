//! Shared serial mechanics. Identity, framing validation, pacing and retries belong to protocols.
#[cfg(windows)]
pub mod cypress;
pub mod serial;
pub mod usb;
