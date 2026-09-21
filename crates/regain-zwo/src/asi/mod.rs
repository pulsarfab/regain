//! ZWO ASI camera backends. Each runs in an isolated regain-device process.
#[cfg(feature = "asi-direct")]
pub mod direct;
#[cfg(feature = "asi-sdk")]
pub mod sdk;
