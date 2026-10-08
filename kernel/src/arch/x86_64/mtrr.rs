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
//! Systems"), and one that did not is said and never refused a boot: the
//! boot processor's are kept as it read them before any other CPU started
//! ([`init`]), and every other CPU reads its own as it comes up and says how
//! they stand beside those ([`compare`]). What a CPU whose registers are on
//! and not the boot processor's costs the machine is its caller's to say
//! ([`any_differs`]).

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

pub use kernel::mtrr::{effective_under_wc, Effective};
use kernel::mtrr::Beside;

use crate::arch::cpu;
use crate::log;

const IA32_MTRRCAP: u32 = 0xFE;
const IA32_MTRR_DEF_TYPE: u32 = 0x2FF;
const IA32_MTRR_PHYSBASE0: u32 = 0x200;

/// A CPU's default-type word and each variable register's
/// `(PHYSBASE, PHYSMASK)`.
type Registers = (u64, Vec<(u64, u64)>);

/// The boot processor's. Written once, by [`init`].
static BOOT: AtomicPtr<Registers> = AtomicPtr::new(core::ptr::null_mut());

/// Some CPU's registers are on and not the boot processor's.
static DIFFERS: AtomicBool = AtomicBool::new(false);

/// This CPU's default-type word and each variable register's
/// `(PHYSBASE, PHYSMASK)`.
fn registers() -> Registers {
    let pairs = (0..(cpu::rdmsr(IA32_MTRRCAP) & 0xFF) as u32)
        .map(|i| (cpu::rdmsr(IA32_MTRR_PHYSBASE0 + i * 2), cpu::rdmsr(IA32_MTRR_PHYSBASE0 + i * 2 + 1)))
        .collect();
    (cpu::rdmsr(IA32_MTRR_DEF_TYPE), pairs)
}

/// Keep the boot processor's registers as firmware handed them over. On the
/// boot processor, before any other CPU starts.
pub fn init() {
    let boot = registers();
    log!("mtrr: the boot processor's range registers: IA32_MTRR_DEF_TYPE {:#x} and {} variable pairs", boot.0, boot.1.len());
    let was = BOOT.swap(Box::into_raw(Box::new(boot)), Ordering::Release);
    assert!(was.is_null(), "mtrr: init ran twice");
}

/// The boot processor's registers, as [`init`] read them.
pub fn boot() -> (u64, &'static [(u64, u64)]) {
    let at = BOOT.load(Ordering::Acquire);
    assert!(!at.is_null(), "mtrr: the boot processor's range registers were asked for before they were read");
    // SAFETY: `init` stored a leaked `Box` once, and nothing frees or writes it.
    let (def_type, pairs) = unsafe { &*at };
    (*def_type, pairs)
}

/// Read this CPU's registers and say how they stand beside the boot
/// processor's. On every other CPU, as it comes up.
pub fn compare(cpu_id: u32) {
    let (boot, (def_type, pairs)) = (boot(), registers());
    match kernel::mtrr::beside(boot, (def_type, &pairs)) {
        Beside::Same => log!("mtrr: cpu{cpu_id}'s range registers are the boot processor's"),
        Beside::Off => log!(
            "mtrr: cpu{cpu_id}'s range registers are off (IA32_MTRR_DEF_TYPE {def_type:#x}) and not the boot processor's: \
             every read this CPU makes is uncached"
        ),
        Beside::Different => {
            DIFFERS.store(true, Ordering::Release);
            let pair = pairs.iter().zip(boot.1).position(|(own, boots)| own != boots);
            log!(
                "mtrr: cpu{cpu_id}'s range registers are on and not the boot processor's: IA32_MTRR_DEF_TYPE {def_type:#x} \
                 beside {:#x}, {} variable pairs beside {}, the first that is not the same {}",
                boot.0,
                pairs.len(),
                boot.1.len(),
                match pair {
                    Some(n) => alloc::format!("pair {n}, {:#x}/{:#x} beside {:#x}/{:#x}", pairs[n].0, pairs[n].1, boot.1[n].0, boot.1[n].1),
                    None => "none".into(),
                },
            );
        }
    }
}

/// Whether any CPU that came up holds registers that are on and not the boot
/// processor's: on such a machine what the boot processor's say of a range
/// is not known of a read another CPU makes there.
pub fn any_differs() -> bool {
    DIFFERS.load(Ordering::Acquire)
}

/// The memory type firmware gave `[base, base + size)` on this CPU; fixed
/// MTRRs (first 1 MiB) are not consulted.
pub fn range_type(base: u64, size: u64) -> Effective {
    let (def_type, pairs) = registers();
    kernel::mtrr::range_type(def_type, pairs, base, base + (size - 1))
}
