//! Camera acquisition and images have a separate contract from scalar samples.
//! Image buffers are immutable, bounded and shared for the lifetime of readers.
pub mod acquisition;
pub mod image;
pub mod json_image;
pub mod properties;
