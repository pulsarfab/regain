//! Camera acquisition and images have a separate contract from scalar samples.
//! Image buffers are immutable, bounded and shared for the lifetime of readers.
pub mod acquisition;
pub mod config;
pub mod image;
pub mod ipc_image;
pub mod ipc_timing;
pub mod json_image;
pub mod native_capture;
pub mod native_owner;
pub mod native_properties;
pub mod native_source;
pub mod properties;
pub mod runtime;
