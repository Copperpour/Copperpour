//! Konnect — cdylib entry point for KiCAD plugin integration.
//!
//! When compiled as a cdylib, this crate exposes a C-compatible ABI that
//! a thin Python action plugin (or KiCAD PCM package) can load.

mod config;
mod ffi;
mod transport;

pub use ffi::*;
