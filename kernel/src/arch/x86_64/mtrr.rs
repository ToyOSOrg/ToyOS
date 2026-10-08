//! What memory type firmware gave a physical range: the range registers,
//! read here and decided by [`kernel::mtrr`].
//!
//! Read-only: firmware owns these registers, the kernel programs none. A
//! mapping with [`CachePolicy::Normal`](crate::mm::policy::CachePolicy)
//! selects PAT entry 0 (WB), so what this module reports is the effective
//! type; the exception is [`effective_under_wc`], where WC outvotes the MTRR
//! instead of deferring to it.

pub use kernel::mtrr::{effective_under_wc, Effective};

use crate::arch::cpu;

const IA32_MTRRCAP: u32 = 0xFE;
const IA32_MTRR_DEF_TYPE: u32 = 0x2FF;
const IA32_MTRR_PHYSBASE0: u32 = 0x200;

/// The memory type firmware gave `[base, base + size)`; fixed MTRRs (first
/// 1 MiB) are not consulted.
pub fn range_type(base: u64, size: u64) -> Effective {
    let pairs = (0..(cpu::rdmsr(IA32_MTRRCAP) & 0xFF) as u32)
        .map(|i| (cpu::rdmsr(IA32_MTRR_PHYSBASE0 + i * 2), cpu::rdmsr(IA32_MTRR_PHYSBASE0 + i * 2 + 1)));
    kernel::mtrr::range_type(cpu::rdmsr(IA32_MTRR_DEF_TYPE), pairs, base, base + (size - 1))
}
