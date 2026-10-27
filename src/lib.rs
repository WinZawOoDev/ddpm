//! ddpm — Dell Display and Peripheral Manager clone for Ubuntu, in Rust.
//!
//! Talks to monitors directly over DDC/CI (MCCS) on `/dev/i2c-*`, and also
//! works through `ddcutil` when it is installed.

pub mod cli;
pub mod config;
pub mod ddcci;
pub mod driver;
pub mod presets;
pub mod vcp;
