//! x86-64's half of `crate::idle`: what this CPU can enter, read on it, the
//! two entries, and which CPU a processor's ACPI UID names.

use core::arch::asm;

use alloc::vec::Vec;
use toyos_userbound::idle::{CpuIdle, Entry};

use super::cpu;
use crate::sync::Lock;

/// The line `MONITOR` arms before an `MWAIT` state: one per CPU that nothing
/// writes, so the wait ends on an interrupt alone, as `hlt`'s does. `MWAIT`
/// enters no state without an armed monitor (SDM Vol. 2B, MWAIT).
#[repr(align(64))]
pub struct Line(#[allow(dead_code, reason = "the line MONITOR watches, which no code reads")] [u8; 64]);

impl Line {
    pub const fn new() -> Self {
        Self([0; 64])
    }
}

/// What this CPU can enter, read on it.
pub fn facts() -> CpuIdle {
    let (_, _, cpuid_1_ecx, _) = cpu::cpuid(1, 0);
    let (cpuid_5_ecx, cpuid_5_edx) = if cpu::cpuid(0, 0).0 >= 5 {
        let (_, _, ecx, edx) = cpu::cpuid(5, 0);
        (ecx, edx)
    } else {
        (0, 0)
    };
    CpuIdle {
        monitor: cpuid_1_ecx & 1 << 3 != 0,
        mwait_extensions: cpuid_5_ecx & 1 != 0,
        mwait_substates: cpuid_5_edx,
        always_running_timer: cpu::leaf_6().0 & 1 << 2 != 0,
        invariant_tsc: cpu::invariant_tsc(),
    }
}

/// Wait in `entry`'s state with interrupts unmasked, as [`super::hw::halt`]
/// waits in C1; called with them masked, and returns once the CPU has left
/// it: for an interrupt, or from `MWAIT` for an event of the processor's own.
///
/// Both keep `sti; hlt`'s guarantee that a wake landing between the decision
/// and the wait is not slept through. `MWAIT` in the `STI` shadow: an interrupt
/// that "would be delivered to software (as if HLT had been executed)" exits
/// it (SDM Vol. 2B, MWAIT). A trapped port read: "Entry via IO read,
/// EFLAGS[IF]==0: The interrupt wakes the core", and with `IF` set it is taken
/// (AMD BKDG 52740 §2.5.3.2.5, the last of AMD's documents to describe this
/// interface's exit); in the shadow it is pending at the read either way.
pub fn enter(entry: Entry) {
    #[cfg(feature = "mask-windows")]
    crate::windows::irqs_unmasking();
    match entry {
        Entry::Mwait { hint } => {
            let line = &crate::cpu_local::here().arch.idle_line;
            // SAFETY: arms a monitor on this CPU's own line, which it reads nothing through; ECX 0 and EDX 0 are MONITOR's only defined extensions and hints.
            unsafe { asm!("monitor", in("rax") line as *const Line, in("ecx") 0u32, in("edx") 0u32, options(nostack, readonly)) };
            // SAFETY: the hint is one leaf 5 enumerates (`toyos_userbound::idle`), ECX 0 asks no extension; enables interrupts and waits, touching no memory.
            unsafe { asm!("sti; mwait", in("eax") hint, in("ecx") 0u32, options(nomem, nostack)) };
        }
        Entry::Port { port } => {
            // SAFETY: a byte read of a port the acpi claim's holder may read itself (`toyos_userbound::idle`), which its firmware names a C-state entry; enables interrupts and waits.
            unsafe { asm!("sti; in al, dx", in("dx") port, out("al") _, options(nomem, nostack)) };
        }
    }
}

/// Each enabled CPU's APIC ID and ACPI Processor UID, as the MADT names them.
static UIDS: Lock<Vec<(u32, u32)>> = Lock::new(Vec::new());

/// Keep the MADT's processor UIDs, for [`cpu_of`].
pub fn note_uids(uids: &[(u32, u32)]) {
    *UIDS.lock() = uids.to_vec();
}

/// The CPU the ACPI Processor UID `uid` names, where this kernel runs one.
pub fn cpu_of(uid: u32) -> Option<u32> {
    let apic = UIDS.lock().iter().find(|&&(_, u)| u == uid).map(|&(apic, _)| apic)?;
    (0..crate::smp::cpu_count()).find(|&cpu| crate::smp::hardware_id(cpu) == apic)
}
