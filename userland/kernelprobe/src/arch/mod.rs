//! What the probes must say in assembly.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;

#[cfg(not(target_arch = "aarch64"))]
compile_error!("kernelprobe is built for AArch64 alone: its probes are the AArch64 guest's");
