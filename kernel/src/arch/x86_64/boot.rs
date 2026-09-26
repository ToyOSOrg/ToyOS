//! The x86-64 steps of the boot: the entry the loader jumps to, and what
//! `kernel_main` asks of this architecture at the points where one differs
//! from another.

use toyos_abi::boot::{KernelArgs, MemoryMapEntry};

use super::{apic, control_regs, idt, ioapic, pat, percpu};
use crate::drivers::acpi::{self, MadtInfo};
use crate::log;
use crate::mm::Region;

/// Entry point: the bootloader jumps here at `PHYS_OFFSET` with `rdi = &KernelArgs`,
/// switches to the kernel's own stack, and calls `kernel_main`.
/// # Safety
/// Only the bootloader may call this, fresh from firmware, with `rdi` holding a live [`KernelArgs`].
#[unsafe(naked)]
#[no_mangle]
pub unsafe extern "C" fn _start(_kernel_args: &KernelArgs) -> ! {
    core::arch::naked_asm!(
        "mov rax, [rdi + 16]",  // kernel_memory_addr
        "add rax, [rdi + 32]",  // + kernel_stack_addr
        "add rax, [rdi + 40]",  // + kernel_stack_size
        "movabs rbx, {phys_offset}",
        "add rax, rbx",
        "mov rsp, rax",
        "call {kernel_main}",
        phys_offset = const crate::PHYS_OFFSET,
        kernel_main = sym crate::kernel_main,
    );
}

/// Before the panel is armed.
///
/// **Before the panel and not after it**: the loader maps the scanout
/// uncacheable, and `panic_console::arm`'s own record is the panel's first
/// paint, so there is no window between arming the panel and painting
/// through it in which to establish a memory type. The write alone, and it
/// logs nothing: the read-back is `pat::check`, in [`after_console`], where a
/// refusal has a channel to reach.
/// The ACPI tables this architecture decodes: the MADT for its CPUs and I/O
/// APICs, the FADT for reset, soft-off and the century register, the HPET for
/// the clock, the MCFG for ECAM and the DMAR for the IOMMU.
pub const ACPI_TABLES: &[&[u8; 4]] = &[b"APIC", b"FACP", b"HPET", b"MCFG", b"DMAR"];

pub fn before_panel() {
    pat::init();
}

/// Once the console and the boot parameter exist.
pub fn after_console(_args: &KernelArgs, _maps: &[MemoryMapEntry]) {
    // The IDT loads inside `interrupts`, much later: a fault here would reach
    // firmware's handlers, so the actuator is refused by name instead.
    if crate::actuator::test_early_fault() {
        panic!("test-early-fault: x86-64 has no vectors of its own this early");
    }
    // After actuator::init, whose table the `control-regs-bench` probe inside
    // this call reads. `pat::init` restored the `CR0` it found, so a firmware
    // `CD` — which would make every mapping uncacheable whatever the PAT
    // says — ends here.
    control_regs::init_cr0(0);

    // The read-back `pat::init` owes, on a boot that now has three channels to
    // carry a refusal.
    pat::check();

    log!("PAT: IA32_PAT={:#018x}, entry {} = {}",
        pat::msr(), pat::WC_ENTRY, pat::entry_name(pat::WC_ENTRY));
}

/// Physical memory only this architecture's boot uses, kept from the
/// allocator: the AP trampoline page.
pub fn reserved() -> Region {
    Region { start: 0x8000, end: 0x9000 }
}

/// What the boot learns bringing interrupts up and hands later steps.
pub struct Platform {
    madt: MadtInfo,
}

/// Interrupt delivery, this CPU's per-CPU block and the syscall gate.
pub fn interrupts(rsdp_addr: u64) -> Platform {
    // `init_bsp` loads the IDT partway through, as early as this CPU's `gs:`
    // allows: a fault in any later phase then diagnoses instead of stopping in
    // a handler the firmware left behind.
    let madt = acpi::parse_madt(rsdp_addr).expect("ACPI: MADT not found");
    // Off the same tables as the MADT, and before the IDT below makes a panic
    // reportable: a panic that can be reported but not ended leaves the machine
    // holding its panel for a hand that may not be in the room.
    acpi::init_reset(rsdp_addr);
    apic::init();
    percpu::init_bsp(apic::id());
    ioapic::init(&madt);
    idt::enable_interrupts();
    super::syscall::init();
    Platform { madt }
}

/// The clock: the TSC, calibrated against the HPET, and the CMOS wall clock.
pub fn clock(args: &KernelArgs) {
    // HPET clock — enables profiling for everything from here on
    let hpet_base = acpi::find_hpet_base(args.rsdp_addr)
        .expect("ACPI: HPET not found");
    super::hpet::calibrate_counter(hpet_base);
    // Century register and time zone both come from ACPI/firmware, not the RTC's own registers.
    let century_reg = match acpi::rtc_century_register(args.rsdp_addr) {
        Ok(reg) => reg,
        Err(e) => {
            log!("ACPI: the FADT is unreadable ({e:?}), so where the RTC keeps its century is unknown too");
            None
        }
    };
    crate::clock::init_wall(century_reg, args.rtc_utc_offset());
}

/// The per-CPU timer, once the clock converts its bound.
pub fn timer() {
    apic::init_timer();
}

/// The platform's own devices that are not PCI functions.
pub fn platform_devices(rsdp_addr: u64) {
    super::i8042::init(rsdp_addr);
}

/// Every other CPU, running.
pub fn start_other_cpus(platform: &Platform, args: &KernelArgs) {
    super::smp::boot_aps(&platform.madt, args.boot_pml4_addr);
}

/// The interrupt-controller selftests an actuator asks for, once the timer ticks.
#[cfg(feature = "boot-actuators")]
pub fn interrupt_selftests() {
    // Needs interrupts on and the timer already ticking: its last assertion is that the interrupt after the spurious one arrives.
    if crate::actuator::lapic_spurious_selftest() {
        idt::spurious::selftest();
    }
    if crate::actuator::unclaimed_vector_selftest() {
        idt::unclaimed::selftest();
    }
}
