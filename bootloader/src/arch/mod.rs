//! The machine the loader runs on, and the only part of it that knows which one.

#[cfg_attr(target_arch = "x86_64", path = "x86_64.rs")]
#[cfg_attr(target_arch = "aarch64", path = "aarch64.rs")]
mod imp;

pub use imp::*;
