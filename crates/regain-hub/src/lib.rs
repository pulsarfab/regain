//! Shared hub policy. Frontends render configuration and translate interfaces;
//! they must not reimplement safety decisions or invent source capabilities.
pub mod config;
pub mod parameters;
pub mod safety;
