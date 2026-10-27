//! What the SDK asks of the CPU itself, one module per architecture.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::{copy, zero};

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{copy, zero};
