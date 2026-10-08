//! What memory type firmware gave a physical range: the range registers,
//! read here and decided by [`kernel::mtrr`].
//!
//! Read-only: firmware owns these registers, the kernel programs none. A
//! mapping with [`CachePolicy::Normal`](crate::mm::policy::CachePolicy)
//! selects PAT entry 0 (WB), so what this module reports is the effective
//! type; the exception is [`effective_under_wc`], where WC outvotes the MTRR
//! instead of deferring to it.
//!
//! They are the registers of the CPU that reads them. Firmware is to leave
//! every processor's the same (Intel SDM Vol. 3A, "MTRR Considerations in MP
//! Systems"); nothing here checks that it did.

use alloc::vec::Vec;

pub use kernel::mtrr::{effective_under_wc, Effective};

use crate::arch::cpu;

const IA32_MTRRCAP: u32 = 0xFE;
const IA32_MTRR_DEF_TYPE: u32 = 0x2FF;
const IA32_MTRR_PHYSBASE0: u32 = 0x200;

/// This CPU's default-type word and each variable register's
/// `(PHYSBASE, PHYSMASK)`.
pub fn registers() -> (u64, Vec<(u64, u64)>) {
    let pairs = (0..(cpu::rdmsr(IA32_MTRRCAP) & 0xFF) as u32)
        .map(|i| (cpu::rdmsr(IA32_MTRR_PHYSBASE0 + i * 2), cpu::rdmsr(IA32_MTRR_PHYSBASE0 + i * 2 + 1)))
        .collect();
    (cpu::rdmsr(IA32_MTRR_DEF_TYPE), pairs)
}

/// The memory type firmware gave `[base, base + size)` on this CPU; fixed
/// MTRRs (first 1 MiB) are not consulted.
pub fn range_type(base: u64, size: u64) -> Effective {
    let (def_type, pairs) = registers();
    kernel::mtrr::range_type(def_type, pairs, base, base + (size - 1))
}
