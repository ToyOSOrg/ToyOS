//! What osbench writes for one architecture: the clock's add chain and a
//! generated program on both systems, and on ToyOS, which has no call that
//! says, the cores CPUs share.

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
mod aarch64;
#[cfg(all(target_arch = "aarch64", target_os = "linux"))]
pub use aarch64::*;

#[cfg(not(any(target_arch = "x86_64", all(target_arch = "aarch64", target_os = "linux"))))]
compile_error!(
    "osbench finds a ToyOS CPU's core by its x86-64 APIC ID, which AArch64 has no EL0 equivalent of: \
     issues/osbench-builds-for-no-aarch64-toyos.md"
);
