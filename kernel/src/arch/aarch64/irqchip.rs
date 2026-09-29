//! The interrupt controller and the timer: a GICv3 — the distributor the
//! MADT names, this CPU's redistributor, and the system-register CPU
//! interface (GIC architecture specification IHI 0069H) — and the generic
//! timer's EL1 virtual timer (Arm ARM K.a, chapter D12), whose PPI the GTDT
//! names.
//!
//! **What this kernel takes is SGIs and the timer's PPI, and nothing else.**
//! No SPI is routed and no LPI exists: a device's interrupt is a message the
//! GICv3 ITS translates, and every device that would take one is either a
//! driver the small-kernel track moves out of the kernel or a claimed function,
//! which the SMMUv3 of the port's stage 6 must translate first
//! (`super::msi_message`).
//!
//! **The virtual timer, not the physical**: it is the one EL1 owns outright —
//! under a hypervisor the physical one traps — and with `CNTVOFF_EL2` written
//! zero by the entry from EL2 the two count alike. It is level-triggered, so a
//! handler that neither re-arms nor stops it takes it again at once.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use toyos_acpi::MadtEntry;
use toyos_gicv3::{packed_affinity, FRAME};

use super::{cpu, percpu};
use crate::drivers::acpi::direct_phys;
use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::{DirectMap, Mmio};
use crate::hw::MIN_ONE_SHOT;

/// Every INTID this kernel names, in one space: the SGIs it raises, then the
/// identities the generic drivers program for a message-signalled interrupt,
/// which `super::msi_message` refuses on this machine.
#[repr(u8)]
pub(super) enum Intid {
    /// Asks a CPU for a scheduler pass: x86-64's kick, which rides the
    /// timer's vector there.
    Kick = 0,
    /// `crate::log::nested`'s delivery, which [`send_self`] raises.
    LogNest,
    /// What `irq-storm` floods this CPU with.
    Storm,
    Hda,
    VirtioSound,
}

pub(super) const SGI_KICK: u32 = Intid::Kick as u32;
#[cfg(feature = "boot-actuators")]
pub(super) const SGI_STORM: u32 = Intid::Storm as u32;

/// `GICD_CTLR`, and its `ARE` (affinity routing — `ARE_NS` as a non-secure
/// access sees it) and Group 1 enable (`EnableGrp1` or `EnableGrp1A`, the
/// same bit either way), and the write-pending bit.
const GICD_CTLR: u64 = 0x0000;
const CTLR_ARE: u32 = 1 << 4;
const CTLR_GRP1: u32 = 1 << 1;
const RWP: u32 = 1 << 31;
/// `GICD_PIDR2.ArchRev`, bits 7:4: 3 is GICv3, 4 is GICv4.
const GICD_PIDR2: u64 = 0xFFE8;

/// A redistributor's frames: `RD_base`, then `SGI_base` 64 KiB above it.
const GICR_CTLR: u64 = 0x0000;
const GICR_TYPER: u64 = 0x0008;
const GICR_WAKER: u64 = 0x0014;
const WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;
const SGI_BASE: u64 = FRAME;
const GICR_IGROUPR0: u64 = SGI_BASE + 0x0080;
const GICR_ISENABLER0: u64 = SGI_BASE + 0x0100;
const GICR_ICENABLER0: u64 = SGI_BASE + 0x0180;
const GICR_ICPENDR0: u64 = SGI_BASE + 0x0280;
const GICR_ICACTIVER0: u64 = SGI_BASE + 0x0380;
const GICR_IPRIORITYR: u64 = SGI_BASE + 0x0400;
const GICR_ICFGR1: u64 = SGI_BASE + 0x0C04;
const GICR_IGRPMODR0: u64 = SGI_BASE + 0x0D00;

/// Every SGI and PPI this kernel takes runs at one priority, below the mask.
const PRIORITY: u8 = 0x80;
/// `ICC_PMR_EL1`: priorities numerically below this are signalled.
const PRIORITY_MASK: u64 = 0xF0;

/// What the GIC answers `ICC_IAR1_EL1` with when nothing is pending for it.
const SPURIOUS: u32 = 1023;

/// The timer's PPI, as the GTDT names it; zero until [`init`].
static TIMER_INTID: AtomicU32 = AtomicU32::new(0);
/// The distributor's frame, for [`log_state`]; zero until [`init`].
static GICD: AtomicU64 = AtomicU64::new(0);

/// Wait until `done`, for at most `1 / per_second` of a second counted at the
/// rate firmware states: the boot has no calibrated clock yet.
fn settles(per_second: u64, what: &str, done: impl Fn() -> bool) {
    let hz = cpu::stated_counter_hz().expect("GIC: CNTFRQ_EL0 states no rate to bound a wait with");
    let until = cpu::counter() + hz / per_second;
    while !done() {
        assert!(cpu::counter() < until, "GIC: {what} did not settle within 1/{per_second} s");
        core::hint::spin_loop();
    }
}

fn read_sysreg_pmr() -> u64 {
    let v: u64;
    // SAFETY: reads `ICC_PMR_EL1`, which `ICC_SRE_EL1.SRE` ([`init`]) makes accessible.
    unsafe { core::arch::asm!("mrs {}, S3_0_C4_C6_0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

/// Bring the distributor, this CPU's redistributor, its CPU interface and its
/// timer up: every SGI and the timer's PPI enabled at [`PRIORITY`], the timer
/// stopped. Interrupts stay masked at `DAIF`; the caller unmasks them.
pub fn init(rsdp_addr: u64) {
    let madt = toyos_acpi::find_table(direct_phys(), rsdp_addr, b"APIC", toyos_acpi::MADT_ENTRIES)
        .unwrap_or_else(|e| panic!("GIC: the MADT is unusable: {e:?}"));
    let me = cpu::hardware_id();
    let (mut gicd, mut own_frame, mut ranges) = (None, None, [(0u64, 0u32); 4]);
    let mut range_count = 0;
    for entry in toyos_acpi::madt_entries(&madt) {
        match entry {
            Ok(MadtEntry::Gicd { base, .. }) => gicd = Some(base),
            Ok(MadtEntry::Gicr { base, length }) => {
                assert!(range_count < ranges.len(), "GIC: the MADT names more redistributor ranges than {}", ranges.len());
                ranges[range_count] = (base, length);
                range_count += 1;
            }
            Ok(MadtEntry::Gicc(gicc)) if packed_affinity(gicc.mpidr) == me && gicc.gicr_base != 0 => {
                own_frame = Some(gicc.gicr_base)
            }
            Ok(_) => {}
            Err(halt) => panic!("GIC: a MADT entry at +{} declares {} bytes of a {}-byte list", halt.at, halt.declared, halt.list_len),
        }
    }
    let gicd = gicd.expect("GIC: the MADT names no distributor");
    let distributor = crate::mm::paging::map_mmio(gicd, FRAME, MmioPolicy::Uncacheable);
    GICD.store(gicd, Relaxed);
    let revision = distributor.read_u32(GICD_PIDR2) >> 4 & 0xF;
    assert!(revision >= 3, "GIC: GICD_PIDR2.ArchRev is {revision}, and this kernel drives a GICv3 or later");

    // The frame is found in the ranges when the GICC entry does not name it:
    // each redistributor says whose it is in `GICR_TYPER`'s affinity.
    let redistributor = match own_frame {
        Some(frame) => crate::mm::paging::map_mmio(frame, 2 * FRAME, MmioPolicy::Uncacheable),
        None => ranges[..range_count]
            .iter()
            .find_map(|&(base, length)| find_redistributor(base, u64::from(length), me))
            .unwrap_or_else(|| panic!("GIC: no redistributor answers for MPIDR affinity {me:#x}")),
    };

    // The distributor: affinity routing on, and Group 1 on.
    distributor.write_u32(GICD_CTLR, CTLR_ARE | CTLR_GRP1);
    settles(100, "GICD_CTLR", || distributor.read_u32(GICD_CTLR) & RWP == 0);
    assert!(
        distributor.read_u32(GICD_CTLR) & CTLR_ARE != 0,
        "GIC: GICD_CTLR.ARE reads clear, so the distributor stays in legacy mode and no redistributor is used"
    );

    // Awake, then every SGI and PPI off and cleared of what firmware left.
    let waker = redistributor.read_u32(GICR_WAKER);
    redistributor.write_u32(GICR_WAKER, waker & !WAKER_PROCESSOR_SLEEP);
    settles(100, "GICR_WAKER.ChildrenAsleep", || redistributor.read_u32(GICR_WAKER) & WAKER_CHILDREN_ASLEEP == 0);
    redistributor.write_u32(GICR_ICENABLER0, u32::MAX);
    settles(100, "GICR_CTLR after disabling every SGI and PPI", || redistributor.read_u32(GICR_CTLR) & (1 << 3) == 0);
    redistributor.write_u32(GICR_ICPENDR0, u32::MAX);
    redistributor.write_u32(GICR_ICACTIVER0, u32::MAX);
    redistributor.write_u32(GICR_IGROUPR0, u32::MAX);
    redistributor.write_u32(GICR_IGRPMODR0, 0);
    let priorities = u32::from_ne_bytes([PRIORITY; 4]);
    for word in 0..8 {
        redistributor.write_u32(GICR_IPRIORITYR + word * 4, priorities);
    }

    let timer = toyos_acpi::gtdt(direct_phys(), rsdp_addr)
        .unwrap_or_else(|e| panic!("GIC: the GTDT is unusable: {e:?}"))
        .virtual_el1;
    assert!((16..32).contains(&timer.gsiv), "GIC: the GTDT puts the virtual timer at INTID {}, which is not a PPI", timer.gsiv);
    TIMER_INTID.store(timer.gsiv, Relaxed);
    // Each PPI's two bits in `GICR_ICFGR1`: 0b10 edge, 0b00 level.
    let edge = u32::from(timer.edge()) << (2 * (timer.gsiv - 16) + 1);
    redistributor.write_u32(GICR_ICFGR1, edge);

    stop_timer_hardware();
    let enabled = 1 << SGI_KICK | 1 << timer.gsiv;
    #[cfg(feature = "boot-actuators")]
    let enabled = enabled | 1 << Intid::LogNest as u32 | 1 << SGI_STORM;
    redistributor.write_u32(GICR_ISENABLER0, enabled);

    // The CPU interface in system registers, as the declaration says.
    // SAFETY: writes `ICC_SRE_EL1`, which touches no memory.
    unsafe {
        core::arch::asm!(
            "msr S3_0_C12_C12_5, {}",
            "isb",
            in(reg) super::control_regs::ICC_SRE,
            options(nomem, nostack, preserves_flags),
        );
    }
    let sre: u64;
    // SAFETY: reads `ICC_SRE_EL1`.
    unsafe { core::arch::asm!("mrs {}, S3_0_C12_C12_5", out(reg) sre, options(nomem, nostack, preserves_flags)) };
    // `SRE` alone: `DFB` and `DIB` are RAO/WI or RAZ/WI as the implementation,
    // or a hypervisor under it, chooses, and this kernel uses neither bypass.
    assert!(sre & 1 != 0, "GIC: ICC_SRE_EL1 reads {sre:#x}, so the CPU interface is not in system registers");

    // The CPU interface: the priority mask open above `PRIORITY`, one
    // priority drop and deactivation per EOI, Group 1 on.
    // SAFETY: GICv3 CPU interface registers, which `ICC_SRE_EL1.SRE` makes
    // system registers; none touches memory.
    unsafe {
        core::arch::asm!(
            "msr S3_0_C4_C6_0, {pmr}",
            "msr S3_0_C12_C12_4, xzr",
            "msr S3_0_C12_C12_7, {one}",
            "isb",
            pmr = in(reg) PRIORITY_MASK,
            one = in(reg) 1u64,
            options(nomem, nostack, preserves_flags),
        );
    }
    assert_eq!(read_sysreg_pmr(), PRIORITY_MASK, "GIC: ICC_PMR_EL1 did not take {PRIORITY_MASK:#x}");
    log!(
        "GIC: v{revision} distributor at {gicd:#x}, this CPU's redistributor at {:#x}; SGIs and the virtual \
         timer's PPI {} ({}) enabled at priority {PRIORITY:#x}",
        redistributor.addr() - crate::mm::PHYS_OFFSET,
        timer.gsiv,
        if timer.edge() { "edge" } else { "level" },
    );
}

/// The redistributor frame in `[base, base + length)` whose affinity is `me`.
fn find_redistributor(base: u64, length: u64, me: u32) -> Option<Mmio> {
    let range = crate::mm::paging::map_mmio(base, length, MmioPolicy::Uncacheable);
    let offset = toyos_gicv3::find_redistributor(length, me, |at| range.read_u64(at + GICR_TYPER))?;
    Some(Mmio::new(DirectMap::from_phys(base + offset), 2 * FRAME))
}

/// The INTID the GIC hands this CPU, or `None` when it answered spurious.
pub(super) fn acknowledge() -> Option<u32> {
    let intid: u64;
    // SAFETY: reads `ICC_IAR1_EL1`, which acknowledges the highest pending
    // Group 1 interrupt; the caller ends every one it is given.
    unsafe { core::arch::asm!("mrs {}, S3_0_C12_C12_0", out(reg) intid, options(nomem, nostack, preserves_flags)) };
    let intid = (intid & 0xFF_FFFF) as u32;
    (intid != SPURIOUS).then_some(intid)
}

/// Drop the running priority and deactivate `intid`.
pub(super) fn end(intid: u32) {
    // SAFETY: writes `ICC_EOIR1_EL1` with an INTID [`acknowledge`] handed out.
    unsafe { core::arch::asm!("msr S3_0_C12_C12_1, {}", in(reg) u64::from(intid), options(nomem, nostack, preserves_flags)) };
}

/// The timer's INTID, which [`init`] took from the GTDT.
pub(super) fn timer_intid() -> u32 {
    TIMER_INTID.load(Relaxed)
}

/// Raise SGI `intid` on the CPU whose packed affinity is `target`.
fn sgi(intid: u32, target: u32) {
    let value = toyos_gicv3::sgi1r(intid, target);
    // SAFETY: writes `ICC_SGI1R_EL1`, which raises an SGI and touches no
    // memory; the `ISB` sends it before whatever follows.
    unsafe { core::arch::asm!("msr S3_0_C12_C11_5, {}", "isb", in(reg) value, options(nomem, nostack, preserves_flags)) };
}

/// Wake `cpu` so it runs a scheduler pass. Before the port's stage 5 the boot
/// CPU is the only one, so the only `cpu` there is is this one.
pub fn kick_cpu(cpu: u32) {
    assert_eq!(cpu, percpu::cpu_id(), "irqchip: a kick for cpu{cpu}, and other CPUs are the port's stage 5");
    sgi(SGI_KICK, cpu::hardware_id());
}

pub fn kick_all_but_self() {
    let me = percpu::cpu_id();
    for cpu in (0..super::smp::cpu_count()).filter(|&cpu| cpu != me) {
        kick_cpu(cpu);
    }
}

/// Raise `vector` — an SGI's INTID — on this CPU.
pub fn send_self(vector: u8) {
    sgi(u32::from(vector), cpu::hardware_id());
}

/// A pseudo-NMI: an interrupt at a priority `DAIF.I` does not mask, which
/// needs `ICC_PMR_EL1` priority masking in place of `DAIF` everywhere.
pub fn send_nmi(_cpu: u32) {
    owed!("a pseudo-NMI", "no stage yet")
}

/// Nothing to stop: before the port's stage 5 the boot CPU is the only one running.
pub fn stop_other_cpus() {}

/// `CNTV_CTL_EL0.ENABLE`; `IMASK` stays clear.
const TIMER_ENABLE: u64 = 1;
/// `CNTV_CTL_EL0.ISTATUS`: the timer's condition is met.
#[cfg(feature = "boot-actuators")]
const TIMER_ISTATUS: u64 = 1 << 2;

/// [`MIN_ONE_SHOT`] in counter ticks, and never none.
fn floor_ticks() -> u64 {
    crate::clock::counter_ticks(MIN_ONE_SHOT.nanos()).max(1)
}

/// The only write of the comparator: fire `ticks` counter ticks from now, or
/// after [`MIN_ONE_SHOT`] if that is longer, and remember the span as what an
/// EL1 fire re-arms with.
fn arm_ticks(ticks: u64) {
    let ticks = ticks.max(floor_ticks());
    percpu::set_armed_ticks(ticks);
    // SAFETY: the EL1 virtual timer's comparator and control; CPACR has
    // nothing to say about them and `CNTKCTL_EL1` keeps EL0 out.
    unsafe {
        core::arch::asm!(
            "msr cntv_cval_el0, {cval}",
            "msr cntv_ctl_el0, {enable}",
            "isb",
            cval = in(reg) cpu::counter() + ticks,
            enable = in(reg) TIMER_ENABLE,
            options(nomem, nostack, preserves_flags),
        );
    }
}

fn stop_timer_hardware() {
    // SAFETY: the EL1 virtual timer's control, cleared: it asserts nothing.
    unsafe { core::arch::asm!("msr cntv_ctl_el0, xzr", "isb", options(nomem, nostack, preserves_flags)) };
}

/// This CPU's timer, armed to fire `nanos` from now, or after [`MIN_ONE_SHOT`] if that is longer.
pub fn arm_one_shot(nanos: u64) {
    arm_ticks(crate::clock::counter_ticks(nanos));
    crate::trace::trace(crate::trace::Kind::TimerArm, nanos as u32);
}

/// This CPU's timer, armed to fire within `nanos`: sooner than it is armed
/// for, or armed if it is stopped.
pub fn arm_within(nanos: u64) {
    let want = crate::clock::counter_ticks(nanos);
    let remaining = match percpu::armed_ticks() {
        0 => want,
        _ => {
            let cval: u64;
            // SAFETY: reads the EL1 virtual timer's comparator.
            unsafe { core::arch::asm!("mrs {}, cntv_cval_el0", out(reg) cval, options(nomem, nostack, preserves_flags)) };
            cval.saturating_sub(cpu::counter())
        }
    };
    arm_ticks(want.min(remaining));
}

/// Stop the timer: no interrupt until it is armed again.
pub fn stop_timer() {
    percpu::set_armed_ticks(0);
    stop_timer_hardware();
    crate::trace::trace(crate::trace::Kind::TimerStop, 0);
}

/// A timer interrupt taken: armed again for what it was last armed for, or
/// stopped if it was stopped — the one thing that deasserts it.
pub(super) fn rearm() {
    match percpu::armed_ticks() {
        0 => stop_timer_hardware(),
        ticks => arm_ticks(ticks),
    }
}

/// `timer-floor`: this CPU's timer made due with interrupts masked, then
/// asked to fire within a quantum, which leaves it nothing to fire within;
/// then [`FLOOR_FIRES`] of its interrupts taken at EL1 with them open. A
/// re-arm shorter than [`MIN_ONE_SHOT`] is what an EL1 fire repeats, and one
/// that re-fires on its own return leaves this CPU no progress to say
/// anything with.
#[cfg(feature = "boot-actuators")]
pub fn floor_selftest() {
    const FLOOR_FIRES: u32 = 100;
    let _guard = crate::arch::IrqGuard::close();
    arm_one_shot(0);
    let due = || {
        let ctl: u64;
        // SAFETY: reads the EL1 virtual timer's control.
        unsafe { core::arch::asm!("mrs {}, cntv_ctl_el0", out(reg) ctl, options(nomem, nostack, preserves_flags)) };
        ctl & TIMER_ISTATUS != 0
    };
    settles(100, "the timer armed for its floor", due);
    arm_within(toyos_sched::fair::QUANTUM_NS);
    let (armed, floor) = (percpu::armed_ticks(), floor_ticks());
    let before = percpu::kernel_timer_fires();
    cpu::enable_interrupts();
    while percpu::kernel_timer_fires().wrapping_sub(before) < FLOOR_FIRES {
        core::hint::spin_loop();
    }
    cpu::disable_interrupts();
    stop_timer();
    let verdict = if armed >= floor { "PASS" } else { "FAIL" };
    log!("timer-floor: {verdict} armed={armed} floor={floor} ticks: {FLOOR_FIRES} fires taken at EL1 re-armed for the floor");
}
