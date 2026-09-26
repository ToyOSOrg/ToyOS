//! The C runtime's architecture-specific pieces, one module per architecture,
//! each answering the same names: the process entry, the three copies and
//! fills that must not be written in Rust (the compiler lowers a Rust loop that
//! copies back into a call to `memcpy`), and the square roots.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::*;
