//! Camera acquisition and images have a separate contract from scalar samples.
//! Image buffers are immutable, bounded and shared for the lifetime of readers.
pub mod image;
