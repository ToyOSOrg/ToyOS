//! The performance envelope's registers. AArch64 declares no performance
//! request, so there is no proof to read one with and every claim is refused.

use toyos_abi::perf::{CpuRegisters, PackageRegisters};

/// Uninhabited: nothing on this architecture can hold one.
pub enum Declared {}

pub fn declared() -> Result<Declared, &'static str> {
    Err("AArch64 declares no performance request: this kernel programs none of its CPUs' \
         performance controls")
}

pub fn read_cpu(declared: &Declared) -> CpuRegisters {
    match *declared {}
}

pub fn read_package(declared: &Declared) -> PackageRegisters {
    match *declared {}
}

pub fn diverge(declared: &Declared, _: u32) {
    match *declared {}
}
