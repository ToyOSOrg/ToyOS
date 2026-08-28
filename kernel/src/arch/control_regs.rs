//! What `CR0`, `CR4` and `IA32_EFER` hold on every CPU in this machine. One
//! declaration, applied by the BSP and every AP and checked on each; nothing
//! else may write any of the three. Each register is written whole: `CR0`
//! and `EFER` are constants, `CR4` is required bits plus whatever optional
//! bits this CPU offers. `EFER.NXE` lets bit 63 of a paging entry mean *not
//! executable* ([`Prot`](crate::mm::paging::Prot)).

use core::sync::atomic::{AtomicU64, Ordering};

use super::cpu;
use crate::log;

/// `IA32_EFER`, SDM Vol. 3A §2.2.1. Address from Vol. 4 Table 2-2.
mod efer {
    pub const MSR: u32 = 0xC000_0080;
    pub const SCE: u64 = 1 << 0;
    pub const LME: u64 = 1 << 8;
    /// CPU-set on entering long mode; read-only, so excluded from this value.
    pub const LMA: u64 = 1 << 10;
    pub const NXE: u64 = 1 << 11;
}

/// `CR0`, SDM Vol. 3A §2.5.
mod cr0 {
    pub const PE: u64 = 1 << 0;
    pub const MP: u64 = 1 << 1;
    pub const ET: u64 = 1 << 4;
    pub const NE: u64 = 1 << 5;
    pub const WP: u64 = 1 << 16;
    pub const NW: u64 = 1 << 29;
    pub const CD: u64 = 1 << 30;
    pub const PG: u64 = 1 << 31;
}

/// `CR4`, SDM Vol. 3A §2.5.
mod cr4 {
    pub const DE: u64 = 1 << 3;
    pub const PAE: u64 = 1 << 5;
    pub const MCE: u64 = 1 << 6;
    pub const OSFXSR: u64 = 1 << 9;
    pub const OSXMMEXCPT: u64 = 1 << 10;
    pub const LA57: u64 = 1 << 12;
    pub const UMIP: u64 = 1 << 11;
    pub const FSGSBASE: u64 = 1 << 16;
    pub const PCIDE: u64 = 1 << 17;
    pub const SMEP: u64 = 1 << 20;
    pub const SMAP: u64 = 1 << 21;
}

/// `CR0` on every CPU: `TS` stays clear because lazy FP switching would leak
/// a register file across `#NM`; `AM` stays clear because Ring 3 has no `#AC` path.
pub const CR0: u64 = cr0::PE | cr0::MP | cr0::ET | cr0::NE | cr0::WP | cr0::PG;

/// `CR4` bits every CPU must have. `DE` is zero legacy, not need — this kernel
/// touches no debug register. `FSGSBASE` because context switch uses
/// `rdfsbase`/`wrfsbase` unconditionally.
const CR4_REQUIRED: u64 = cr4::DE
    | cr4::PAE
    | cr4::MCE
    | cr4::OSFXSR
    | cr4::OSXMMEXCPT
    | cr4::FSGSBASE;

/// `CR4` bits this kernel takes when the CPU offers them (checked against CPUID first: an undefined bit is `#GP`).
const CR4_OPTIONAL: u64 = cr4::SMEP | cr4::SMAP | cr4::PCIDE | cr4::UMIP;

/// `IA32_EFER` on every CPU: `SCE`, `LME`, `NXE`. `SCE` is declared only
/// here, never by `arch::syscall::init`, so one register keeps one owner.
pub const EFER: u64 = efer::SCE | efer::LME | efer::NXE;

/// The declaration as the BSP computed it. Zero means not yet declared — also [`pcid_active`]'s correct answer before then.
static DECLARED_CR4: AtomicU64 = AtomicU64::new(0);

/// Puts this CPU's `CR0` into [`CR0`]. Must run before
/// [`pat::init`](super::pat::init), whose no-fill window depends on `CD` being live.
pub fn init_cr0(cpu_id: u32) {
    let before = bench::sample();
    if !skipped(cpu_id) {
        let live = cpu::read_cr0();
        if live & (cr0::CD | cr0::NW) != 0 {
            // SDM Vol. 3A §11.5.3's no-fill sequence: `CD` set, `NW` clear,
            // then write-back-invalidate — required when crossing cache states.
            // SAFETY: only `CD`/`NW` change in `write_cr0`; `wbinvd` runs inside
            // the no-fill window the write just opened (SDM Vol. 3A §11.5.3).
            unsafe {
                cpu::write_cr0((live | cr0::CD) & !cr0::NW);
                cpu::wbinvd();
            }
        }
        // SAFETY: `CR0`'s value is this file's declaration, argued in its own
        // doc comment.
        unsafe { cpu::write_cr0(CR0) };
    }
    bench::report(cpu_id, before);
}

/// Puts this CPU's `CR4` and `EFER` into the declaration and checks all
/// three against it. Must run after [`init_cr0`] and before `arch::syscall::init`, which needs `SCE` set.
pub fn init(cpu_id: u32) {
    let declared = declaration(cpu_id);
    if !skipped(cpu_id) {
        // SAFETY: `write_cr4` faults only on an undefined bit, on clearing `PAE`
        // in long mode, or on `PCIDE` with a nonzero PCID — `declaration` checked
        // the first two and both callers use PCID 0; `wrmsr` writes [`EFER`], whose
        // bits `declaration` has just confirmed this CPU defines.
        unsafe {
            cpu::write_cr4(declared);
            cpu::wrmsr(efer::MSR, EFER);
        }
        if declared & cr4::SMAP != 0 {
            // Nothing in this kernel sets `RFLAGS.AC`, so this is the only
            // `clac` the kernel needs.
            cpu::clac();
        }
    }
    self_check(cpu_id, declared);
}

/// Whether the declaration carries `PCIDE`, and therefore whether `INVPCID` is this machine's flush.
pub fn pcid_active() -> bool {
    DECLARED_CR4.load(Ordering::Acquire) & cr4::PCIDE != 0
}

/// Proof that this machine's declaration carries `PCIDE`, so `INVPCID` is
/// not `#UD` here. Zero-sized: an `Option<PcidActive>` costs nothing extra.
/// Never stales: `PCIDE`, once declared, is never cleared.
pub struct PcidActive(());

impl PcidActive {
    /// `Some` where the declaration carries `PCIDE`, `None` where it does not.
    pub fn ask() -> Option<Self> {
        pcid_active().then_some(Self(()))
    }
}

/// What this CPU says [`CR4_REQUIRED`] and [`CR4_OPTIONAL`] come to, checked
/// against what the BSP said. Recomputed per CPU rather than trusted from
/// the BSP, so a divergent machine names the CPU instead of faulting blind.
fn declaration(cpu_id: u32) -> u64 {
    let have = supported();
    let missing = CR4_REQUIRED & !have;
    assert!(
        missing == 0,
        "control_regs: cpu{cpu_id} lacks CR4 bits {missing:#x} that this kernel requires",
    );
    let declared = CR4_REQUIRED | (have & CR4_OPTIONAL);

    // `SYSCALL` and `NX` are `CPUID.80000001H:EDX` bits 11 and 20 (SDM Vol.
    // 2A Table 3-8); the extended leaf must exist before they mean anything.
    let (max_ext, _, _, _) = cpu::cpuid(0x8000_0000, 0);
    let ext_edx = if max_ext >= 0x8000_0001 { cpu::cpuid(0x8000_0001, 0).3 } else { 0 };
    assert!(
        ext_edx & (1 << 11) != 0,
        "control_regs: cpu{cpu_id} has no SYSCALL/SYSRET, which is this kernel's only \
         way into and out of Ring 3",
    );
    assert!(
        ext_edx & (1 << 20) != 0,
        "control_regs: cpu{cpu_id} has no NX bit, so no mapping this kernel writes could \
         be made non-executable and W^X would silently not exist",
    );

    // Changing `LA57` with paging on is `#GP`, so this only ever reads it.
    let live = cpu::read_cr4();
    assert!(
        live & cr4::LA57 == 0,
        "control_regs: cpu{cpu_id} is in 5-level paging and this kernel's page tables are 4-level",
    );

    match DECLARED_CR4.compare_exchange(0, declared, Ordering::Release, Ordering::Acquire) {
        Ok(_) => declared,
        Err(published) => {
            assert!(
                published == declared,
                "control_regs: cpu{cpu_id} computes cr4={declared:#010x} and the machine \
                 declared {published:#010x} — its CPUs do not offer the same features",
            );
            declared
        }
    }
}

/// The `CR4` bits this CPU will accept, as CPUID reports them.
fn supported() -> u64 {
    const CPUID_1_EDX: [(u32, u64); 5] = [
        (2, cr4::DE),
        (6, cr4::PAE),
        (7, cr4::MCE),
        (24, cr4::OSFXSR),
        (25, cr4::OSXMMEXCPT),
    ];
    const CPUID_7_EBX: [(u32, u64); 3] =
        [(0, cr4::FSGSBASE), (7, cr4::SMEP), (20, cr4::SMAP)];
    const CPUID_7_ECX: [(u32, u64); 1] = [(2, cr4::UMIP)];

    let (max_leaf, _, _, _) = cpu::cpuid(0, 0);
    let (_, _, ecx1, edx1) = cpu::cpuid(1, 0);
    // A leaf above the maximum answers with the highest basic leaf's data
    // instead of faulting, so an unguarded read here would misreport bits.
    let (_, ebx7, ecx7, _) = if max_leaf >= 7 { cpu::cpuid(7, 0) } else { (0, 0, 0, 0) };

    let mut have = 0;
    for (bit, flag) in CPUID_1_EDX {
        if edx1 & (1 << bit) != 0 {
            have |= flag;
        }
    }
    for (bit, flag) in CPUID_7_EBX {
        if ebx7 & (1 << bit) != 0 {
            have |= flag;
        }
    }
    for (bit, flag) in CPUID_7_ECX {
        if ecx7 & (1 << bit) != 0 {
            have |= flag;
        }
    }
    // PCID without INVPCID is not worth having: nothing could flush by ASID.
    if ecx1 & (1 << 17) != 0 && ebx7 & (1 << 10) != 0 {
        have |= cr4::PCIDE;
    }
    have
}

/// Logs what this CPU holds, then asserts it against the declaration —
/// logged first so a failing CPU still leaves its value in the log.
fn self_check(cpu_id: u32, declared_cr4: u64) {
    let live_cr0 = cpu::read_cr0();
    let live_cr4 = cpu::read_cr4();
    let live_efer = cpu::rdmsr(efer::MSR);
    log!(
        "control_regs: cpu{} cr0={:#010x} cr4={:#010x} efer={:#06x}{}{}{}{}{}",
        cpu_id,
        live_cr0,
        live_cr4,
        live_efer,
        opt(live_cr4, cr4::SMEP, " smep"),
        opt(live_cr4, cr4::SMAP, " smap"),
        opt(live_cr4, cr4::PCIDE, " pcid"),
        opt(live_cr4, cr4::UMIP, " umip"),
        opt(live_efer, efer::NXE, " nx"),
    );
    // Order matches the declaration: `CR0`, `CR4`, `EFER` — a diverging CPU
    // usually diverges in all three, and the first assert is what a reader sees.
    assert!(
        live_cr0 == CR0,
        "control_regs: cpu{cpu_id} holds cr0={live_cr0:#010x}, the declaration is {CR0:#010x}",
    );
    assert!(
        live_cr4 == declared_cr4,
        "control_regs: cpu{cpu_id} holds cr4={live_cr4:#010x}, the declaration is \
         {declared_cr4:#010x}",
    );
    // `LMA` is excluded: clearing it means the CPU itself left long mode.
    assert!(
        live_efer & !efer::LMA == EFER && live_efer & efer::LMA != 0,
        "control_regs: cpu{cpu_id} holds efer={live_efer:#06x}, the declaration is \
         {EFER:#06x} plus the CPU's own LMA",
    );
}

fn opt(value: u64, bit: u64, name: &'static str) -> &'static str {
    if value & bit != 0 { name } else { "" }
}

/// Cycles the caching probe took. Bare metal only — QEMU models no cache and
/// KVM never holds `CD` — read via `--kernel-param control-regs-bench`.
#[cfg(feature = "boot-actuators")]
mod bench {
    use super::cpu;
    use crate::log;

    /// Bigger than any L1, inside every L2 this kernel targets.
    const LINES: usize = 4096;
    const STRIDE: usize = 8;
    static PROBE: [u64; LINES * STRIDE] = [0; LINES * STRIDE];

    pub fn sample() -> u64 {
        if !crate::actuator::control_regs_bench() {
            return 0;
        }
        let start = cpu::rdtsc();
        let mut acc = 0u64;
        let mut i = 0;
        while i < PROBE.len() {
            // SAFETY: `i < PROBE.len()` keeps the index in bounds.
            acc = acc.wrapping_add(unsafe { core::ptr::read_volatile(&raw const PROBE[i]) });
            i += STRIDE;
        }
        let end = cpu::rdtsc();
        core::hint::black_box(acc);
        end.wrapping_sub(start)
    }

    pub fn report(cpu_id: u32, before: u64) {
        if !crate::actuator::control_regs_bench() {
            return;
        }
        let cold = sample();
        let warm = sample();
        log!(
            "control_regs: cpu{} probe {} lines: pre={} cold={} warm={} cycles",
            cpu_id, LINES, before, cold, warm,
        );
    }
}

#[cfg(not(feature = "boot-actuators"))]
mod bench {
    pub fn sample() -> u64 {
        0
    }
    pub fn report(_cpu_id: u32, _before: u64) {}
}

/// The negative control: leaves an AP holding what `INIT` left it, since no
/// QEMU flag can stage a divergent control register any other way.
fn skipped(cpu_id: u32) -> bool {
    crate::actuator::no_ap_control_regs() && cpu_id != 0
}

