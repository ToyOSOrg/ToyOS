//! Measurement scout, never landed: four general-purpose counters per CPU,
//! programmed at bring-up for TLB misses in both rings and read from Ring 3 by
//! `RDPMC` (`CR4.PCE`, which [`programme`] decides alongside the events).
//!
//! Intel: architectural performance monitoring v2+ with four counters,
//! `IA32_PERFEVTSELx`/`IA32_PMCx` (SDM Vol. 3B §20.2), Ice Lake-family event
//! encodings (Tiger Lake: Intel perfmon `TGL` core events). AMD: family 17h and
//! 19h legacy `PerfEvtSel0-3`/`PerfCtr0-3` (APM Vol. 2 §13.2.1), PPR 17h model
//! 60h event encodings. `RDPMC` index 0-3 reads counter 0-3 on both.

use super::cpu;
use crate::log;

/// One counter's selection: event, unit mask, and its name in the log.
#[derive(Clone, Copy)]
pub struct Event {
    pub event: u16,
    pub umask: u8,
    pub name: &'static str,
}

const fn ev(event: u16, umask: u8, name: &'static str) -> Event {
    Event { event, umask, name }
}

const INTEL: [Event; 4] = [
    ev(0x08, 0x0E, "dtlb_load_walk_completed"),
    ev(0x49, 0x0E, "dtlb_store_walk_completed"),
    ev(0x85, 0x0E, "itlb_walk_completed"),
    ev(0x08, 0x10, "dtlb_load_walk_pending_cycles"),
];

const AMD: [Event; 4] = [
    ev(0x45, 0xFF, "l1_dtlb_miss"),
    ev(0x45, 0xF0, "l2_dtlb_miss"),
    ev(0x84, 0x00, "l1_itlb_miss_l2_hit"),
    ev(0x85, 0x0F, "l2_itlb_miss"),
];

struct Msrs {
    select: [u32; 4],
    counter: [u32; 4],
}

const INTEL_MSRS: Msrs = Msrs { select: [0x186, 0x187, 0x188, 0x189], counter: [0xC1, 0xC2, 0xC3, 0xC4] };
const AMD_MSRS: Msrs = Msrs {
    select: [0xC001_0000, 0xC001_0001, 0xC001_0002, 0xC001_0003],
    counter: [0xC001_0004, 0xC001_0005, 0xC001_0006, 0xC001_0007],
};

/// `IA32_PERF_GLOBAL_CTRL`'s enables for general-purpose counters 0-3.
pub const INTEL_GLOBAL_GP: u64 = 0xF;
const IA32_PERF_GLOBAL_CTRL: u32 = 0x38F;

/// The events and MSRs this CPU has a programme for, decided from CPUID alone;
/// the scout programs agree with it by the same rule (`scout_tlb`'s `pmc_programme`).
fn programme() -> Option<(&'static [Event; 4], &'static Msrs, bool)> {
    let (sig, _, _, _) = cpu::cpuid(1, 0);
    let family = ((sig >> 8) & 0xF) + if (sig >> 8) & 0xF == 0xF { (sig >> 20) & 0xFF } else { 0 };
    match cpu::vendor() {
        toyos_cpuvuln::Vendor::Intel => {
            if cpu::cpuid(0, 0).0 < 0x0A {
                return None;
            }
            let (eax, _, _, _) = cpu::cpuid(0x0A, 0);
            let version = eax & 0xFF;
            let gp = (eax >> 8) & 0xFF;
            (version >= 2 && gp >= 4).then_some((&INTEL, &INTEL_MSRS, true))
        }
        toyos_cpuvuln::Vendor::Amd if family == 0x17 || family == 0x19 => Some((&AMD, &AMD_MSRS, false)),
        _ => None,
    }
}

/// Whether this CPU gets `CR4.PCE`: exactly where it gets counters.
pub fn wants_pce() -> bool {
    programme().is_some()
}

/// Program this CPU's four counters, zeroed, counting in both rings.
pub fn arm(cpu_id: u32) {
    let Some((events, msrs, intel)) = programme() else {
        log!("scout-pmc: cpu{cpu_id} has no counter programme; RDPMC is not enabled");
        return;
    };
    const USR: u64 = 1 << 16;
    const OS: u64 = 1 << 17;
    const EN: u64 = 1 << 22;
    for (i, e) in events.iter().enumerate() {
        let select = u64::from(e.event & 0xFF)
            | u64::from(e.umask) << 8
            | USR
            | OS
            | EN
            | u64::from(e.event >> 8) << 32;
        // SAFETY: the counter and its select register exist on this vendor and
        // family (`programme`), and the value holds only the select's defined fields.
        unsafe {
            cpu::wrmsr(msrs.select[i], 0);
            cpu::wrmsr(msrs.counter[i], 0);
            cpu::wrmsr(msrs.select[i], select);
        }
    }
    if intel {
        // SAFETY: architectural since perfmon v2, which `programme` required.
        unsafe { cpu::wrmsr(IA32_PERF_GLOBAL_CTRL, cpu::rdmsr(IA32_PERF_GLOBAL_CTRL) | INTEL_GLOBAL_GP) };
    }
    if cpu_id == 0 {
        let names: [&str; 4] = core::array::from_fn(|i| events[i].name);
        log!("scout-pmc: rdpmc 0..3 = {names:?}");
    }
}

/// The CPUID bits the TLB twins turn on: `PCID` (01H:ECX[17]), `INVPCID`
/// (07H:EBX[10]), `PGE` (01H:EDX[13]).
pub fn log_cpuid(cpu_id: u32) {
    let (sig, _, ecx1, edx1) = cpu::cpuid(1, 0);
    let (_, ebx7, _, _) = cpu::leaf_7();
    log!(
        "scout-cpuid: cpu{cpu_id} signature={sig:#x} pcid={} invpcid={} pge={} cr4={:#010x}",
        (ecx1 >> 17) & 1,
        (ebx7 >> 10) & 1,
        (edx1 >> 13) & 1,
        cpu::read_cr4(),
    );
}
