//! TLB invalidation. AArch64 broadcasts it in hardware: a `TLBI …IS` reaches
//! every CPU in the inner-shareable domain, and the `DSB ISH` after it
//! returns once every one of them has dropped the entry (Arm ARM K.a,
//! D8.13.4). So nothing here sends an interrupt or waits for an
//! acknowledgement, and [`shootdown`] is one instruction.
//!
//! Every operation below is bracketed the same way: `DSB ISHST` so the table
//! write it answers for is visible to every walker first, then the `TLBI`,
//! then `DSB ISH` for its completion and `ISB` so this CPU's next fetch walks
//! afresh.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::invalidation::Origin;

/// Issuer-side census, as x86-64's counts it; there is no receiver side.
static ISSUED: [AtomicU64; Origin::COUNT] = [const { AtomicU64::new(0) }; Origin::COUNT];
/// Total at the last print; process exit logs once per batch.
static REPORTED: AtomicU64 = AtomicU64::new(0);

macro_rules! tlbi {
    ($op:literal, $operand:expr) => {
        // SAFETY: TLB maintenance and barriers change no memory; dropping a
        // translation only makes the next access walk the tables again.
        unsafe {
            core::arch::asm!(
                "dsb ishst",
                concat!("tlbi ", $op, ", {}"),
                "dsb ish",
                "isb",
                in(reg) $operand,
                options(nostack, preserves_flags),
            )
        }
    };
}

/// Every translation `asid` holds for the 4 KiB page at `va`, on every CPU.
pub(super) fn page(asid: u16, va: u64) {
    tlbi!("vae1is", u64::from(asid) << 48 | (va >> 12) & 0xFFF_FFFF_FFFF);
}

/// Every translation `asid` holds, on every CPU.
pub(super) fn asid(asid: u16) {
    tlbi!("aside1is", u64::from(asid) << 48);
}

/// A global (kernel) translation of the 4 KiB page at `va`, on every CPU.
pub(super) fn kernel_page(va: u64) {
    tlbi!("vaae1is", (va >> 12) & 0xFFF_FFFF_FFFF);
}

/// Every EL1&0 translation, on every CPU.
fn all() {
    // SAFETY: as `tlbi!`'s.
    unsafe {
        core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack, preserves_flags));
    }
}

/// Every translation every CPU holds, dropped before this returns — what a
/// returned ASID needs before it is issued again.
pub fn shootdown(origin: Origin) {
    ISSUED[origin as usize].fetch_add(1, Ordering::Relaxed);
    all();
}

/// Nothing to answer: no CPU waits on another's acknowledgement here.
pub fn poll() {}

/// One `tlb:` line when the counts moved, at process exit.
pub fn log_census() {
    let mut counts = [0u64; Origin::COUNT];
    for (slot, count) in ISSUED.iter().zip(counts.iter_mut()) {
        *count = slot.load(Ordering::Relaxed);
    }
    let total: u64 = counts.iter().sum();
    if total == 0 || REPORTED.swap(total, Ordering::Relaxed) == total {
        return;
    }
    struct Fields([u64; Origin::COUNT]);
    impl core::fmt::Display for Fields {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            for (name, count) in Origin::NAMES.iter().zip(self.0) {
                write!(f, " {name}={count}")?;
            }
            Ok(())
        }
    }
    crate::log!("tlb: broadcast invalidations={total}{}", Fields(counts));
}

/// x86-64's measures an IPI round trip, and there is none here.
#[cfg(feature = "boot-actuators")]
pub fn bench() {
    panic!("tlb-shootdown-bench: AArch64 invalidates by broadcast, so there is no IPI round trip to measure");
}

/// x86-64's delays an acknowledgement, and there is none here: refused.
#[cfg(feature = "test-actuators")]
pub fn debug_arm_ack_delay(_nanos: u64) -> u64 {
    toyos_abi::syscall::SyscallError::NotSupported.to_u64()
}

/// x86-64's delays an acknowledgement, and there is none here: refused.
#[cfg(feature = "test-actuators")]
pub fn debug_disarm_ack_delay() -> u64 {
    toyos_abi::syscall::SyscallError::NotSupported.to_u64()
}
