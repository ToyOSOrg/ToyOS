//! The AArch64 steps of the boot: the entries firmware and the loader jump
//! to, and what `kernel_main` asks of this architecture at the points where
//! one differs from another.
//!
//! **The entries.** Every CPU arrives at its physical address with the MMU of
//! the level it runs at off, so nothing firmware left in a translation regime —
//! its tables, their execute-never attributes, the regime `HCR_EL2.E2H` chose —
//! is walked under either entry. The loader hands the boot CPU over at the
//! level firmware ran it, EL2 or EL1, with the kernel image and [`KernelArgs`]
//! cleaned to the point of coherency, and jumps to [`_start`] with
//! `x0 = &KernelArgs`. PSCI starts every other CPU at [`ap_start`], at the
//! level firmware gives an operating system, with `x0` its [`ApStart`]'s
//! physical address. Each names a root table and branches to
//! [`apply_declaration`], which writes the [`control_regs`](super::control_regs)
//! declaration whole: at EL2 it writes `HCR_EL2` first, halts in a named
//! refusal unless it reads back as declared, then programs EL1's registers with
//! the MMU already on and drops with `ERET`; at EL1 it programs them and turns
//! the MMU on. Either way the entry resumes at its link address in the view at
//! `PHYS_OFFSET`, installs the vectors on its own stack, and calls
//! `kernel_main` or `smp::ap_entry`.

use core::mem::offset_of;

use toyos_abi::boot::{KernelArgs, MemoryMapEntry};
use toyos_acpi::MadtEntry;

use super::control_regs as regs;
use super::smp::ApStart;
use crate::drivers::acpi::direct_phys;
use crate::log;
use crate::mm::Region;

/// Entry point: the loader jumps here at its physical address with this
/// level's MMU off, and `x0 = &KernelArgs`.
/// # Safety
/// Only the loader may call this, fresh from firmware, with `x0` holding a live [`KernelArgs`].
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn _start(_kernel_args: &KernelArgs) -> ! {
    core::arch::naked_asm!(
        "mov x19, x0",
        // x20 = the stack's top, physical: kernel image + stack offset + stack size.
        "ldr x20, [x19, #{kernel_memory}]",
        "ldr x1, [x19, #{stack_offset}]",
        "add x20, x20, x1",
        "ldr x1, [x19, #{stack_size}]",
        "add x20, x20, x1",
        "ldr x3, [x19, #{root}]",
        "ldr x22, =1f",
        "b {declare}",
        "1:",
        "ldr x1, ={phys_offset}",
        "add x20, x20, x1",
        "mov sp, x20",
        "add x19, x19, x1",
        "adrp x1, {entry_el}",
        "str x21, [x1, :lo12:{entry_el}]",
        "bl {install}",
        "mov x0, x19",
        "mov x29, xzr",
        "mov x30, xzr",
        "bl {kernel_main}",
        kernel_memory = const offset_of!(KernelArgs, kernel_memory_addr),
        stack_offset = const offset_of!(KernelArgs, kernel_stack_addr),
        stack_size = const offset_of!(KernelArgs, kernel_stack_size),
        root = const offset_of!(KernelArgs, boot_pml4_addr),
        declare = sym apply_declaration,
        phys_offset = const crate::PHYS_OFFSET,
        entry_el = sym regs::ENTRY_EL,
        install = sym super::trap::install,
        kernel_main = sym crate::kernel_main,
    );
}

/// Where PSCI `CPU_ON` starts every other CPU, at its physical address with
/// its MMU off and `x0` the physical address of its [`ApStart`].
/// # Safety
/// Only firmware may enter this, as `super::smp::start` asked it to.
#[unsafe(naked)]
pub(super) unsafe extern "C" fn ap_start() -> ! {
    core::arch::naked_asm!(
        "mov x19, x0",
        "ldr x20, [x19, #{stack_top}]",
        "ldr x3, [x19, #{root}]",
        "ldr x22, =1f",
        "b {declare}",
        "1:",
        "mov sp, x20",
        "ldr x1, ={phys_offset}",
        "add x19, x19, x1",
        "bl {install}",
        "mov x0, x19",
        "mov x1, x21",
        "mov x29, xzr",
        "mov x30, xzr",
        "bl {ap_entry}",
        stack_top = const offset_of!(ApStart, stack_top),
        root = const offset_of!(ApStart, root),
        declare = sym apply_declaration,
        phys_offset = const crate::PHYS_OFFSET,
        install = sym super::trap::install,
        ap_entry = sym super::smp::ap_entry,
    );
}

/// The declaration written on this CPU and its MMU turned on under the root
/// in `x3`, whichever level it was entered at; then a branch to the link
/// address in `x22` at EL1 on `SP_EL1`, with `x21` the level entered at.
/// Keeps `x19`, `x20` and `x22`. Entered by a branch from an entry running at
/// its physical address, and never returns.
/// # Safety
/// Only [`_start`] and [`ap_start`] branch here, with `x3` a root that maps the
/// kernel image at both its physical and its link address.
#[unsafe(naked)]
unsafe extern "C" fn apply_declaration() -> ! {
    core::arch::naked_asm!(
        // x2 = TCR_EL1 whole, x4 = MAIR_EL1, x5 = SCTLR_EL1.
        "ldr x2, ={tcr}",
        "mrs x1, id_aa64mmfr0_el1",
        "and x1, x1, #0xf",
        "orr x2, x2, x1, lsl #{ips}",
        "ldr x4, ={mair}",
        "ldr x5, ={sctlr}",
        "mrs x21, CurrentEL",
        "lsr x21, x21, #2",
        "cmp x21, #2",
        "b.eq 2f",
        "cmp x21, #1",
        "b.ne 9f",
        // EL1: the declaration, then translation on.
        "msr mair_el1, x4",
        "msr tcr_el1, x2",
        "msr ttbr0_el1, x3",
        "msr ttbr1_el1, x3",
        "ldr x1, ={cpacr}",
        "msr cpacr_el1, x1",
        "mov x1, #{cntkctl}",
        "msr cntkctl_el1, x1",
        "tlbi vmalle1",
        "dsb nsh",
        "isb",
        "msr sctlr_el1, x5",
        "isb",
        "br x22",
        // EL2: `HCR_EL2` first, since with `E2H` set every `_el1` name
        // below is an EL2 register; refused unless it reads back as declared.
        // EL2's own MMU is off, so the regime `E2H` chooses changes under no
        // walk. Then EL1's registers with the MMU on, EL2's others, and the drop.
        "2:",
        "ldr x1, ={hcr}",
        "msr hcr_el2, x1",
        "isb",
        "mrs x6, hcr_el2",
        "cmp x6, x1",
        "b.ne {refuse_hcr}",
        // `ICC_SRE_EL2`, whose `Enable` lets EL1 write its own `ICC_SRE_EL1`
        // (`super::irqchip::init_cpu`). Skipped where `ID_AA64PFR0_EL1.GIC` names
        // no system-register interface: there the write is an undefined
        // instruction under firmware's vectors and ends the boot silently,
        // while EL1's own access fails under this kernel's, which report it.
        "mrs x1, id_aa64pfr0_el1",
        "ubfx x1, x1, #24, #4",
        "cbz x1, 4f",
        "mov x1, #{icc_sre_el2}",
        "msr S3_4_C12_C9_5, x1",
        "isb",
        "4:",
        "msr mair_el1, x4",
        "msr tcr_el1, x2",
        "msr ttbr0_el1, x3",
        "msr ttbr1_el1, x3",
        "ldr x1, ={cpacr}",
        "msr cpacr_el1, x1",
        "mov x1, #{cntkctl}",
        "msr cntkctl_el1, x1",
        "msr sctlr_el1, x5",
        "mov x1, #{cnthctl}",
        "msr cnthctl_el2, x1",
        "msr cntvoff_el2, xzr",
        "ldr x1, ={cptr}",
        "msr cptr_el2, x1",
        "tlbi vmalle1",
        "dsb nsh",
        "mov x1, #{spsr}",
        "msr spsr_el2, x1",
        "msr elr_el2, x22",
        "isb",
        "eret",
        // EL3, or anything else: nothing here may run there.
        "9:",
        "wfe",
        "b 9b",
        tcr = const regs::TCR,
        ips = const regs::TCR_IPS_SHIFT,
        mair = const regs::MAIR,
        sctlr = const regs::SCTLR,
        hcr = const regs::HCR_EL2,
        refuse_hcr = sym refused_hcr_el2_readback,
        cnthctl = const regs::CNTHCTL_EL2,
        cptr = const regs::CPTR_EL2,
        cpacr = const regs::CPACR,
        cntkctl = const regs::CNTKCTL,
        icc_sre_el2 = const regs::ICC_SRE_EL2,
        spsr = const regs::SPSR_EL2_TO_EL1,
    );
}

/// Where a CPU entered at EL2 halts when `HCR_EL2` reads back anything but the
/// declaration the entry wrote: `E2H` held set, which the loader refuses by
/// name first (`bootloader/src/arch/aarch64.rs`), or any other bit. There the
/// registers the drop programs are not the ones the declaration names, so the
/// kernel does not run. Nothing can report yet: the console is found
/// after the drop, so the refusal is this symbol, which the halted PC names.
#[unsafe(naked)]
#[no_mangle]
unsafe extern "C" fn refused_hcr_el2_readback() -> ! {
    core::arch::naked_asm!("1:", "wfe", "b 1b")
}

/// The ACPI tables this architecture decodes: the MADT for its GIC and CPUs,
/// the FADT for PSCI and reset, the GTDT for the timer, the SPCR for the
/// console, and the MCFG for ECAM.
pub const ACPI_TABLES: &[&[u8; 4]] = &[b"APIC", b"FACP", b"GTDT", b"SPCR", b"MCFG"];

/// Before the panel is armed: nothing. The loader mapped the scanout with its
/// final memory type, Normal non-cacheable, and `MAIR_EL1` names that type
/// since the entry.
pub fn before_panel() {}

/// Once the console and the boot parameter exist: the declaration read back,
/// and what this boot found — the memory map and the tables the rest of the
/// port reads.
pub fn after_console(args: &KernelArgs, maps: &[MemoryMapEntry]) {
    regs::check(regs::ENTRY_EL.load(core::sync::atomic::Ordering::Relaxed));
    for entry in maps {
        log!("memory: {:#014x}..{:#014x} uefi type {}", entry.start, entry.end, entry.uefi_type);
    }
    log!("memory: {} ranges, as the loader handed them over", maps.len());
    survey(args.rsdp_addr);

    // After the vectors and before anything else can fault: the earliest
    // exception they must report.
    if crate::actuator::test_early_fault() {
        super::cpu::undefined_instruction();
    }
}

/// The MADT's GIC structures and the GTDT's timers, decoded and said: what
/// the interrupt controller and the timer of stage 4 are built from.
fn survey(rsdp_addr: u64) {
    match toyos_acpi::find_table(direct_phys(), rsdp_addr, b"APIC", toyos_acpi::MADT_ENTRIES) {
        Ok(madt) => {
            let (mut cpus, mut enabled) = (0u32, 0u32);
            for item in toyos_acpi::madt_entries(&madt) {
                match item {
                    Ok(MadtEntry::Gicc(gicc)) => {
                        cpus += 1;
                        enabled += u32::from(gicc.enabled);
                        log!(
                            "ACPI: MADT GICC uid={} mpidr={:#x} enabled={} gicr={:#x}",
                            gicc.uid,
                            gicc.mpidr,
                            gicc.enabled,
                            gicc.gicr_base
                        );
                    }
                    Ok(MadtEntry::Gicd { base, version }) => {
                        log!("ACPI: MADT GICD at {base:#x}, GIC version {version}")
                    }
                    Ok(MadtEntry::Gicr { base, length }) => {
                        log!("ACPI: MADT GICR range {base:#x}+{length:#x}")
                    }
                    Ok(MadtEntry::Its { id, base }) => log!("ACPI: MADT ITS {id} at {base:#x}"),
                    Ok(MadtEntry::LocalApic { .. }
                    | MadtEntry::IoApic(_)
                    | MadtEntry::SourceOverride(_)
                    | MadtEntry::Other(_)) => {}
                    Err(halt) => {
                        log!(
                            "ACPI: MADT entry at +{} declares {} bytes of a {}-byte list — stopping",
                            halt.at,
                            halt.declared,
                            halt.list_len
                        );
                        break;
                    }
                }
            }
            log!("ACPI: MADT names {cpus} GIC CPU interfaces, {enabled} enabled");
        }
        Err(e) => log!("ACPI: MADT unusable: {e:?}"),
    }
    match toyos_acpi::gtdt(direct_phys(), rsdp_addr) {
        Ok(gtdt) => log!(
            "ACPI: GTDT timers: EL1 physical GSIV {}, EL1 virtual GSIV {}, EL2 GSIV {} ({})",
            gtdt.non_secure_el1.gsiv,
            gtdt.virtual_el1.gsiv,
            gtdt.el2.gsiv,
            if gtdt.virtual_el1.edge() { "edge" } else { "level" },
        ),
        Err(e) => log!("ACPI: GTDT unusable: {e:?}"),
    }
}

/// Physical memory only this architecture's boot uses: none.
pub fn reserved() -> Region {
    Region { start: 0, end: 0 }
}

/// What the boot learns bringing interrupts up and hands later steps: the
/// other CPUs the MADT names, and how to start them.
pub struct Platform {
    gic: super::irqchip::Gic,
    psci: Option<super::psci::Conduit>,
}

/// Interrupt delivery: this CPU's per-CPU block, the GIC and the timer's
/// interrupt, and interrupts unmasked. The syscall gate is the vectors' own.
pub fn interrupts(rsdp_addr: u64) -> Platform {
    super::percpu::init_bsp();
    let gic = super::irqchip::init(rsdp_addr);
    super::cpu::enable_interrupts();
    Platform { gic, psci: super::psci::init(rsdp_addr) }
}

/// The clock: the generic timer's count, at the rate firmware states in
/// `CNTFRQ_EL0`, which the Arm ARM makes firmware's to program and which is
/// what the counter counts at. No wall clock: `super::rtc` says why.
pub fn clock(_args: &KernelArgs) {
    let hz = super::cpu::stated_counter_hz().expect("clock: CNTFRQ_EL0 states no rate for the generic timer");
    crate::clock::set_counter(super::cpu::counter(), 1_000_000_000_000_000 / hz);
    log!("clock: the generic timer counts at {hz} Hz; no wall clock is read on this architecture");
}

/// Nothing: where the generic timer counts from is firmware's, and no
/// register says it.
pub fn report_counter_origin() {}

/// The per-CPU timer counts the clock's own ticks, so there is nothing to
/// calibrate: it stays stopped until the scheduler first arms it.
pub fn timer() {
    log!("timer: the EL1 virtual timer, PPI {}, stopped until the scheduler arms it", super::irqchip::timer_intid());
}

/// The platform's own devices that are not PCI functions: none this kernel
/// drives on an ACPI Arm machine.
pub fn platform_devices(_rsdp_addr: u64) {}

/// Every other CPU, running.
pub fn start_other_cpus(platform: &Platform, _args: &KernelArgs) {
    super::smp::start(&platform.gic, platform.psci);
}

/// The interrupt-controller selftests an actuator asks for.
#[cfg(feature = "boot-actuators")]
pub fn interrupt_selftests() {
    if crate::actuator::timer_floor() {
        super::irqchip::floor_selftest();
    }
    if crate::actuator::irq_storm() {
        super::trap::storm::run();
    }
    assert!(
        !crate::actuator::lapic_spurious_selftest() && !crate::actuator::unclaimed_vector_selftest(),
        "the local APIC's selftests are x86-64's, and this machine has a GIC"
    );
}
