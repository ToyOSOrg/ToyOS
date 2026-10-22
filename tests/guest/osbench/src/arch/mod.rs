//! What ToyOS's half of `sys` names CPUs by: the architecture's own reading of
//! the CPU a thread runs on and of the cores CPUs share.

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::{core_of, cpu_model, current_cpu, threads_per_core};

#[cfg(not(target_arch = "x86_64"))]
compile_error!(
    "osbench names a ToyOS CPU by its x86-64 APIC ID, and has no AArch64 equivalent: \
     issues/osbench-builds-for-no-aarch64-toyos.md"
);
