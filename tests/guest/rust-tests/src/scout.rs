//! Measurement scout, never landed: what `scout_tlb` and `scout_wake` share —
//! the counter, the clock rate, the diary, and the PMC programme the scout
//! kernel's `arch::x86_64::scout_pmc` decides by the same CPUID rule.

#![allow(dead_code)]

use std::sync::OnceLock;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::clock::nanos_since_boot as clock_nanos;
use toyos_abi::trace::{TraceCursor, TraceRecord};

pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("lfence", "rdtsc", "lfence", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    ((hi as u64) << 32) | lo as u64
}

/// The counter's rate, against the kernel's clock over 20 ms.
pub fn tsc_mhz() -> u64 {
    let t0 = clock_nanos();
    let c0 = rdtsc();
    while clock_nanos() - t0 < 20_000_000 {}
    (rdtsc() - c0) * 1_000 / (clock_nanos() - t0)
}

/// Which of the scout kernel's two programmes this CPU runs, if any: the
/// kernel sets `CR4.PCE` exactly where this answers `Some`.
pub fn pmc_programme() -> Option<&'static str> {
    use core::arch::x86_64::__cpuid_count;
    let leaf0 = unsafe { __cpuid_count(0, 0) };
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&leaf0.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&leaf0.edx.to_le_bytes());
    vendor[8..12].copy_from_slice(&leaf0.ecx.to_le_bytes());
    let sig = unsafe { __cpuid_count(1, 0) }.eax;
    let base = (sig >> 8) & 0xF;
    let family = base + if base == 0xF { (sig >> 20) & 0xFF } else { 0 };
    match &vendor {
        b"GenuineIntel" => {
            if leaf0.eax < 0x0A {
                return None;
            }
            let eax = unsafe { __cpuid_count(0x0A, 0) }.eax;
            ((eax & 0xFF) >= 2 && ((eax >> 8) & 0xFF) >= 4).then_some("intel")
        }
        b"AuthenticAMD" if family == 0x17 || family == 0x19 => Some("amd"),
        _ => None,
    }
}

/// The four counters, or zeros where there is no programme.
pub fn pmcs(have: bool) -> [u64; 4] {
    if !have {
        return [0; 4];
    }
    core::array::from_fn(|i| {
        let lo: u32;
        let hi: u32;
        unsafe {
            core::arch::asm!("rdpmc", in("ecx") i as u32, out("eax") lo, out("edx") hi, options(nomem, nostack));
        }
        ((hi as u64) << 32) | lo as u64
    })
}

/// `later - earlier` for 48-bit counters.
pub fn pmc_delta(later: [u64; 4], earlier: [u64; 4]) -> [u64; 4] {
    core::array::from_fn(|i| later[i].wrapping_sub(earlier[i]) & ((1 << 48) - 1))
}

fn cap() -> &'static SysCap {
    static CAP: OnceLock<SysCap> = OnceLock::new();
    CAP.get_or_init(|| Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability"))
}

/// Every diary record from `cursor` on, and how many were lost before it reached them.
pub fn diary(cursor: &mut TraceCursor) -> (Vec<TraceRecord>, u64) {
    let mut out = Vec::new();
    let mut lost = 0;
    let mut raw = vec![TraceRecord::EMPTY; 512];
    loop {
        let n = cap().trace(cursor, &mut raw).expect("the estate's capability reads the diary");
        lost += cursor.lost();
        out.extend_from_slice(&raw[..n]);
        if n < raw.len() {
            return (out, lost);
        }
    }
}

/// A cursor at the diary's present end.
pub fn diary_now() -> TraceCursor {
    let mut cursor = TraceCursor::new();
    diary(&mut cursor);
    cursor
}

pub fn percentile(sorted: &[u64], p: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(sorted.len() - 1) * p / 100]
}
