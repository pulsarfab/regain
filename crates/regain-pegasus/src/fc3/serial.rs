//! FocusCube3 selection over the shared Pegasus serial transport.
pub use crate::serial::{Port, Serial, read_frame};
pub fn candidates() -> anyhow::Result<Vec<Port>> {
    crate::serial::candidates(0x9000)
}
