//! The AArch64 steps of the boot: the entry the loader jumps to, and what
//! `kernel_main` asks of this architecture at the points where one differs
//! from another.
//!
//! **The entry.** The loader leaves the CPU as firmware ran it — at EL2 or
//! EL1, on firmware's identity tables — cleans the kernel image and its own
//! tables to the point of coherency, and jumps to [`_start`]'s physical
//! address with `x0 = &KernelArgs`. `_start` writes the
//! [`control_regs`](super::control_regs) declaration whole: at EL2 it programs
//! EL1's registers with the MMU already on and drops with `ERET`; at EL1 it
//! turns the MMU off first, so no translation register changes under a live
//! walk. Either way it arrives at the kernel's link address in the view at
//! `PHYS_OFFSET`, on the kernel's own stack, with the vectors installed, and
//! calls `kernel_main`.

use core::mem::offset_of;

use toyos_abi::boot::{KernelArgs, MemoryMapEntry};
use toyos_acpi::MadtEntry;

use super::control_regs as regs;
use crate::drivers::acpi::DirectPhys;
use crate::log;
use crate::mm::Region;

/// Entry point: the loader jumps here at its physical address, MMU on under
/// firmware's identity map, with `x0 = &KernelArgs`.
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
        // x2 = TCR_EL1 whole, x3 = the loader's L0 table, x4 = MAIR_EL1.
        "ldr x2, ={tcr}",
        "mrs x1, id_aa64mmfr0_el1",
        "and x1, x1, #0xf",
        "orr x2, x2, x1, lsl #{ips}",
        "ldr x3, [x19, #{root}]",
        "ldr x4, ={mair}",
        "ldr x5, ={sctlr}",
        "mrs x21, CurrentEL",
        "lsr x21, x21, #2",
        "cmp x21, #2",
        "b.eq 2f",
        "cmp x21, #1",
        "b.ne 9f",
        // EL1: translation off, then the declaration, then translation on.
        "ldr x1, ={sctlr_off}",
        "msr sctlr_el1, x1",
        "isb",
        "msr mair_el1, x4",
        "msr tcr_el1, x2",
        "msr ttbr0_el1, x3",
        "msr ttbr1_el1, x3",
        "msr cpacr_el1, xzr",
        "tlbi vmalle1",
        "dsb nsh",
        "isb",
        "msr sctlr_el1, x5",
        "isb",
        "ldr x1, =3f",
        "br x1",
        // EL2: EL1's registers with the MMU on, EL2's as declared, then drop.
        "2:",
        "msr mair_el1, x4",
        "msr tcr_el1, x2",
        "msr ttbr0_el1, x3",
        "msr ttbr1_el1, x3",
        "msr cpacr_el1, xzr",
        "msr sctlr_el1, x5",
        "ldr x1, ={hcr}",
        "msr hcr_el2, x1",
        "mov x1, #{cnthctl}",
        "msr cnthctl_el2, x1",
        "msr cntvoff_el2, xzr",
        "ldr x1, ={cptr}",
        "msr cptr_el2, x1",
        "tlbi vmalle1",
        "dsb nsh",
        "mov x1, #{spsr}",
        "msr spsr_el2, x1",
        "ldr x1, =3f",
        "msr elr_el2, x1",
        "isb",
        "eret",
        // At the link address, EL1, MMU on.
        "3:",
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
        // EL3, or anything else: nothing here may run there.
        "9:",
        "wfe",
        "b 9b",
        kernel_memory = const offset_of!(KernelArgs, kernel_memory_addr),
        stack_offset = const offset_of!(KernelArgs, kernel_stack_addr),
        stack_size = const offset_of!(KernelArgs, kernel_stack_size),
        root = const offset_of!(KernelArgs, boot_pml4_addr),
        tcr = const regs::TCR,
        ips = const regs::TCR_IPS_SHIFT,
        mair = const regs::MAIR,
        sctlr = const regs::SCTLR,
        sctlr_off = const regs::SCTLR_MMU_OFF,
        hcr = const regs::HCR_EL2,
        cnthctl = const regs::CNTHCTL_EL2,
        cptr = const regs::CPTR_EL2,
        spsr = const regs::SPSR_EL2_TO_EL1,
        phys_offset = const crate::PHYS_OFFSET,
        entry_el = sym regs::ENTRY_EL,
        install = sym super::trap::install,
        kernel_main = sym crate::kernel_main,
    );
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
    regs::check();
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
    match toyos_acpi::find_table(DirectPhys, rsdp_addr, b"APIC", toyos_acpi::MADT_ENTRIES) {
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
    match toyos_acpi::gtdt(DirectPhys, rsdp_addr) {
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

/// What the boot learns bringing interrupts up and hands later steps.
pub struct Platform {
    never: core::convert::Infallible,
}

/// Interrupt delivery, this CPU's per-CPU block and the syscall gate.
pub fn interrupts(_rsdp_addr: u64) -> Platform {
    owed!("interrupt delivery", "stage 4")
}

/// The clock: the generic timer's counter at `CNTFRQ_EL0`.
pub fn clock(_args: &KernelArgs) {
    owed!("the clock", "stage 4")
}

/// The per-CPU timer.
pub fn timer() {
    owed!("the timer", "stage 4")
}

/// The platform's own devices that are not PCI functions: none this kernel
/// drives on an ACPI Arm machine.
pub fn platform_devices(_rsdp_addr: u64) {}

/// Every other CPU, running.
pub fn start_other_cpus(platform: &Platform, _args: &KernelArgs) {
    match platform.never {}
}

/// The interrupt-controller selftests an actuator asks for.
#[cfg(feature = "boot-actuators")]
pub fn interrupt_selftests() {
    owed!("the interrupt controller", "stage 4")
}
