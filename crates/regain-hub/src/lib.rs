//! Shared hub policy. Frontends render configuration and translate interfaces;
//! they must not reimplement safety decisions or invent source capabilities.
pub mod alpaca;
pub mod config;
pub mod description;
pub mod parameters;
pub mod readout;
pub mod safety;
pub mod safety_output;
pub mod source;
pub mod switch;
pub mod weather;
