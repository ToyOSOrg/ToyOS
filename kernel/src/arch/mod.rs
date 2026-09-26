//! The machine, and the only part of this kernel that knows which one.
//!
//! One implementation per architecture, chosen here at compile time and
//! nowhere else: generic code names `crate::arch::…` and never an
//! architecture, and a `cfg` on `target_arch` appears in this file and inside
//! the implementation it selects. Every item one implementation exports, the
//! other exports with the same meaning — a missing one is a compile error in
//! the architecture that lacks it, not a fallback.

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
