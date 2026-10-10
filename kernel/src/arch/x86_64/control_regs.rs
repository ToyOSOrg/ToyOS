//! What `CR0`, `CR4`, `IA32_EFER` and the performance request hold on every
//! CPU in this machine. One declaration, applied by the BSP and every AP and
//! checked on each; nothing else may write any of them. Each register is
//! written whole: `CR0` and `EFER` ([`kernel::efer`]) are constants, `CR4` is
//! required bits plus whatever optional bits this CPU offers. `EFER.NXE` lets
//! bit 63 of a paging entry mean *not executable*
//! ([`Prot`](crate::mm::policy::Prot)).
//!
//! The performance request is HWP's: `IA32_HWP_INTERRUPT` where it exists,
//! `IA32_PM_ENABLE`, `IA32_HWP_REQUEST` as `toyos_cpuvuln::hwp_request` derives
//! it from this CPU's own registers, `IA32_HWP_REQUEST_PKG` and
//! `IA32_ENERGY_PERF_BIAS`, on a machine whose CPUs have every one of them,
//! and none on one that does not: refused by name once, firmware's values
//! standing. `IA32_MISC_ENABLE`'s turbo bit is not declared: that register's
//! other bits are model-specific and firmware's, and writing it whole would
//! decide them.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use toyos_cpuvuln::HwpFacts;

use kernel::efer::{self, DECLARED as EFER};

use super::cpu;
use crate::log;

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

/// The performance request's registers, SDM Vol. 4.
mod hwp {
    pub const PLATFORM_INFO: u32 = 0xCE;
    pub const ENERGY_PERF_BIAS: u32 = 0x1B0;
    pub const PM_ENABLE: u32 = 0x770;
    pub const HWP_CAPABILITIES: u32 = 0x771;
    pub const HWP_REQUEST_PKG: u32 = 0x772;
    pub const HWP_INTERRUPT: u32 = 0x773;
    pub const HWP_REQUEST: u32 = 0x774;
}

/// `IA32_PM_ENABLE` on every CPU: HWP on, which only a reset turns off.
const PM_ENABLE: u64 = 1;

/// `IA32_HWP_INTERRUPT` on every CPU that has it: nothing in this kernel takes
/// an HWP notification.
const HWP_INTERRUPT: u64 = 0;

/// `IA32_HWP_REQUEST_PKG` on every CPU: the widest range at the request's own
/// preference. Inert, since no CPU's request sets package control, and
/// declared so that firmware does not decide it either.
const HWP_REQUEST_PKG: u64 = 0x01 | 0xff << 8 | toyos_cpuvuln::HWP_EPP << 24;

/// `IA32_ENERGY_PERF_BIAS` on every CPU: Linux's `ENERGY_PERF_BIAS_NORMAL`
/// (`arch/x86/include/asm/msr-index.h:858`).
const ENERGY_PERF_BIAS: u64 = 6;

/// `CR0` on every CPU: `TS` stays clear because lazy FP switching would leak
/// a register file across `#NM`; `AM` stays clear because Ring 3 has no `#AC` path.
pub const CR0: u64 = cr0::PE | cr0::MP | cr0::ET | cr0::NE | cr0::WP | cr0::PG;

/// `CR4` bits every CPU must have. `DE` is zero legacy, not need — this kernel
/// touches no debug register. `FSGSBASE` is [`CR4_FORBIDDEN`], not here.
const CR4_REQUIRED: u64 =
    cr4::DE | cr4::PAE | cr4::MCE | cr4::OSFXSR | cr4::OSXMMEXCPT;

/// `CR4` bits this kernel takes when the CPU offers them (checked against CPUID first: an undefined bit is `#GP`).
const CR4_OPTIONAL: u64 = cr4::SMEP | cr4::SMAP | cr4::PCIDE | cr4::UMIP;

/// The `CR4` bit no shipping CPU may hold: `FSGSBASE` gives Ring 3 `WRGSBASE`,
/// and `GS.base` aims the first memory access of every kernel entry
/// (`arch::percpu::gs`) — a user-writable `GS.base` is a Ring 3 arbitrary write.
/// The kernel's FS base uses `IA32_FS_BASE` (`cpu::write_fs_base`) instead.
const CR4_FORBIDDEN: u64 = cr4::FSGSBASE;

// Forbidden in the declaration, and never optional.
const _: () = assert!(CR4_REQUIRED & CR4_FORBIDDEN == 0);
const _: () = assert!(CR4_OPTIONAL & CR4_FORBIDDEN == 0);

/// The declaration as the BSP computed it. Zero means not yet declared — also [`pcid_active`]'s correct answer before then.
static DECLARED_CR4: AtomicU64 = AtomicU64::new(0);

/// CPUs that have run [`self_check`] against the declaration and survived it.
/// Counted rather than inferred from the roster, so [`report`]'s line is
/// evidence of the check having run and not a restatement of the CPU count.
static CHECKED: AtomicU64 = AtomicU64::new(0);

/// Puts this CPU's `CR0` into [`CR0`]. [`pat::init`](super::pat::init)
/// restores the `CR0` it finds, so a firmware `CD` survives that write and is
/// cleared here: every AP runs this first, and the BSP runs it after, because
/// the BSP's `pat::init` has to precede the panel it would report a refusal on.
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

/// Whether the machine's declaration carries the performance request: the
/// BSP's verdict, which every AP must reach too.
static HWP: AtomicU8 = AtomicU8::new(HWP_UNDECIDED);
const HWP_UNDECIDED: u8 = 0;
const HWP_DECLARED: u8 = 1;
const HWP_REFUSED: u8 = 2;

/// Whether this CPU gets a performance request, from CPUID alone, and whether
/// it has `IA32_HWP_INTERRUPT`: the machine's answer, a CPU that disagrees
/// named.
fn hwp_declared(cpu_id: u32) -> Option<toyos_cpuvuln::Hwp> {
    let (cpuid_6_eax, cpuid_6_ecx) = cpu::leaf_6();
    let verdict = toyos_cpuvuln::hwp(&HwpFacts {
        vendor: cpu::vendor(),
        signature: cpu::cpuid(1, 0).0,
        cpuid_6_eax,
        cpuid_6_ecx,
        cpuid_7_0_edx: cpu::leaf_7().3,
    });
    let mine = if verdict.is_ok() { HWP_DECLARED } else { HWP_REFUSED };
    match HWP.compare_exchange(HWP_UNDECIDED, mine, Ordering::Release, Ordering::Acquire) {
        Ok(_) => {
            if let Err(refusal) = verdict {
                log!("control_regs: no performance request is declared: {}", refusal.reason());
            }
        }
        Err(machine) => assert!(
            machine == mine,
            "control_regs: cpu{cpu_id} {} a performance request and the BSP {} one",
            if mine == HWP_DECLARED { "can hold" } else { "cannot hold" },
            if machine == HWP_DECLARED { "declared" } else { "refused" },
        ),
    }
    verdict.ok()
}

/// Puts this CPU's `CR4` and `EFER` into the declaration and checks both
/// against it. Must run after [`init_cr0`] and before `arch::syscall::init`,
/// which needs `SCE` set.
pub fn init(cpu_id: u32) {
    let declared = declaration(cpu_id);
    if !skipped(cpu_id) {
        // SAFETY: `write_cr4` faults only on an undefined bit, on clearing `PAE`
        // in long mode, or on `PCIDE` with a nonzero PCID — `declaration` checked
        // the first two and both callers use PCID 0; `wrmsr` writes [`EFER`], whose
        // bits `declaration` has just confirmed this CPU defines and whose `LMA`
        // is the one this CPU, in long mode, already holds.
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

/// Puts this CPU's performance request into the declaration and checks it.
/// Must run after [`init`] and after this CPU's IDT is loaded, so a fault in
/// its `wrmsr` or `rdmsr` panics by name.
///
/// The interrupt is cleared and HWP enabled before any other HWP register is
/// touched, the request's input included: SDM Vol. 3B §17.4.2 (253669-093US),
/// "Additional MSRs associated with HWP may only be accessed after HWP is
/// enabled, with the exception of IA32_HWP_INTERRUPT and MSR_PPERF", and
/// `intel_pstate_hwp_enable`'s order.
pub fn init_performance(cpu_id: u32) {
    let Some(hwp) = hwp_declared(cpu_id) else { return };
    // SAFETY: `hwp_declared` admitted this CPU only where CPUID enumerates HWP,
    // and `IA32_HWP_INTERRUPT` where it enumerates that too; both values hold
    // only defined bits.
    unsafe {
        if hwp.notify {
            cpu::wrmsr(hwp::HWP_INTERRUPT, HWP_INTERRUPT);
        }
        cpu::wrmsr(hwp::PM_ENABLE, PM_ENABLE);
    }
    let request = toyos_cpuvuln::hwp_request(cpu::rdmsr(hwp::HWP_CAPABILITIES), cpu::rdmsr(hwp::PLATFORM_INFO));
    // SAFETY: HWP is enabled, so its registers are live; CPUID enumerated EPP,
    // the package request and EPB, and every value holds only defined bits.
    unsafe {
        cpu::wrmsr(hwp::HWP_REQUEST, request);
        cpu::wrmsr(hwp::HWP_REQUEST_PKG, HWP_REQUEST_PKG);
        cpu::wrmsr(hwp::ENERGY_PERF_BIAS, ENERGY_PERF_BIAS);
    }
    hwp_check(cpu_id, request, hwp.notify);
}

/// The power envelope this CPU runs under, where the performance request is
/// declared: read only after this CPU's [`init_performance`], which enabled HWP.
pub fn envelope() -> Option<crate::counters::Envelope> {
    (HWP.load(Ordering::Acquire) == HWP_DECLARED).then(|| crate::counters::Envelope {
        hwp_request: cpu::rdmsr(hwp::HWP_REQUEST),
        hwp_request_pkg: cpu::rdmsr(hwp::HWP_REQUEST_PKG),
        energy_perf_bias: cpu::rdmsr(hwp::ENERGY_PERF_BIAS),
    })
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

    let (_, _, ecx1, edx1) = cpu::cpuid(1, 0);
    let (_, ebx7, ecx7, _) = cpu::leaf_7();

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
    assert!(
        live_efer == EFER,
        "control_regs: cpu{cpu_id} holds efer={live_efer:#06x}, the declaration is {EFER:#06x}",
    );
    CHECKED.fetch_add(1, Ordering::Relaxed);
}

/// [`self_check`] for the performance request, logged first for the same
/// reason; the line carries the request's two inputs, so a reader can
/// recompute it.
fn hwp_check(cpu_id: u32, request: u64, notify: bool) {
    let pm_enable = cpu::rdmsr(hwp::PM_ENABLE);
    let live = cpu::rdmsr(hwp::HWP_REQUEST);
    let pkg = cpu::rdmsr(hwp::HWP_REQUEST_PKG);
    let epb = cpu::rdmsr(hwp::ENERGY_PERF_BIAS);
    let interrupt = notify.then(|| cpu::rdmsr(hwp::HWP_INTERRUPT));
    log!(
        "control_regs: cpu{} pm_enable={} hwp_request={:#010x} hwp_request_pkg={:#010x} epb={} \
         hwp_interrupt={:x?} hwp_capabilities={:#010x} platform_info={:#018x}",
        cpu_id,
        pm_enable,
        live,
        pkg,
        epb,
        interrupt,
        cpu::rdmsr(hwp::HWP_CAPABILITIES),
        cpu::rdmsr(hwp::PLATFORM_INFO),
    );
    for (name, holds, declared) in [
        ("pm_enable", Some(pm_enable), PM_ENABLE),
        ("hwp_request", Some(live), request),
        ("hwp_request_pkg", Some(pkg), HWP_REQUEST_PKG),
        ("epb", Some(epb), ENERGY_PERF_BIAS),
        ("hwp_interrupt", interrupt, HWP_INTERRUPT),
    ] {
        let Some(holds) = holds else { continue };
        assert!(
            holds == declared,
            "control_regs: cpu{cpu_id} holds {name}={holds:#x}, the declaration is {declared:#x}",
        );
    }
}

/// How many CPUs hold the declaration, said once after the last of them has
/// been checked. A divergent CPU panics inside [`self_check`], so what this
/// line adds is the *count*: a CPU that never reached [`init`] at all is
/// invisible to a per-CPU assert and shows here as a number below the roster's.
///
/// Reported and not asserted: an AP that echoed past `boot_aps`' budget was
/// checked without being committed, and the machine is declared to boot with
/// the CPUs it got rather than to crash on that one.
pub fn report(cpus: u32) {
    log!(
        "control_regs: {} of {cpus} cpus hold cr0={:#010x} cr4={:#010x} efer={:#06x}",
        CHECKED.load(Ordering::Relaxed),
        CR0,
        DECLARED_CR4.load(Ordering::Acquire),
        EFER,
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

