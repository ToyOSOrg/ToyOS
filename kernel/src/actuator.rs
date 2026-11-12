//! Test-only hooks a boot parameter stages; absent `boot-actuators` every
//! accessor is `const fn … { false }`, so a shipping kernel carries none of
//! this and a call site folds to the shipped branch. It is our own
//! bootloader's string and crosses no trust boundary: an unknown token
//! panics by name rather than being ignored.

#[cfg(feature = "boot-actuators")]
use core::sync::atomic::{AtomicU64, Ordering};

macro_rules! actuators {
    ($( $(#[$doc:meta])* $name:ident = $wire:literal; )*) => {
        #[cfg(feature = "boot-actuators")]
        const NAMES: &[&str] = &[$($wire),*];

        $(
            $(#[$doc])*
            #[cfg(feature = "boot-actuators")]
            #[inline(always)]
            pub fn $name() -> bool {
                const AT: (usize, u64) = bit_of($wire);
                ARMED[AT.0].load(Ordering::Relaxed) & AT.1 != 0
            }

            $(#[$doc])*
            #[cfg(not(feature = "boot-actuators"))]
            // An accessor's only caller can live behind the same cfg, which a binary crate still flags as dead code.
            #[allow(dead_code)]
            #[inline(always)]
            pub const fn $name() -> bool {
                false
            }
        )*
    };
}

actuators! {
    /// Panic between arming the on-screen console and `mm::init`.
    test_early_panic = "test-early-panic";

    /// Take an undefined-instruction exception right after the architecture's
    /// console step, where it has one to take there: the earliest fault the
    /// exception vectors must report. AArch64 installs its vectors in the entry;
    /// x86-64 loads its IDT later, and panics by name instead.
    test_early_fault = "test-early-fault";

    /// Send every copy of at least 64 bytes whose ends differ modulo 4 KiB
    /// highest first, whatever the CPU (`arch::x86_64::copy`). Judged by
    /// `a_user_copy_highest_first_faults_or_ends_as_bad_address`.
    copies_go_down = "copies-go-down";

    /// Have the first syscall null SS, force a switch, and report whether it reloaded — the
    /// AMD `SYSRET` SS-attributes workaround's only guest-observable proof.
    sysret_ss_probe = "sysret-ss-probe";

    /// Declare the i8042's command port to the kernel at boot, as a FADT
    /// reset register at 0x64 is, so an `isa` claim on its row is refused by
    /// name. Judged by `isa_claim_refused_on_a_kernel_port`.
    isa_port_declared = "isa-port-declared";


    /// Send one NMI from the idle loop, and return from its handler via `iretq` with a second NMI already pending.
    nmi_nested = "nmi-nested";

    /// Enter vector 2 at CPL 0 once, with a decoy block live as the user's
    /// GS base. Judged by `nmi_finds_the_kernels_gs` and
    /// `nmi_under_a_user_gs_base`.
    nmi_user_gs = "nmi-user-gs";

    /// Walk the PCI capability list, window check and parse over crafted config-space layouts at init.
    pci_cap_selftest = "pci-cap-selftest";

    /// Raise the local APIC's spurious vector on this CPU once.
    lapic_spurious_selftest = "lapic-spurious-selftest";

    /// Raise a vector no `idt_vectors!` row claims on this CPU once.
    unclaimed_vector_selftest = "unclaimed-vector-selftest";

    /// Tick the timer at a fixed period while this CPU floods itself with
    /// interrupts.
    irq_storm = "irq-storm";

    /// Make this CPU's timer due with interrupts masked, ask it to fire within
    /// a quantum, and take its interrupts with them open.
    timer_floor = "timer-floor";

    /// Leave every IOMMU unit as firmware may hand one over, just before this
    /// kernel takes it: a VT-d unit queueing, translating and remapping
    /// through tables of its own, an AMD-Vi unit blocking every device's DMA
    /// with its command buffer and event log running. Judged by
    /// `iommu_firmware_left` and `amdvi_firmware_left`.
    iommu_firmware_left = "iommu-firmware-left";

    /// QEMU's two `iommu-testdev` asked three things of the AMD-Vi unit:
    /// one on a domain of its own writes where it maps, the other on the
    /// identity domain writes to memory, then where that domain maps
    /// nothing, and the machine halts on the record. Judged by
    /// `amdvi_selftest`.
    amdvi_selftest = "amdvi-selftest";

    /// Leave every AP holding the CR0/CR4 that INIT left it.
    no_ap_control_regs = "no-ap-control-regs";

    /// Make one direct-map leaf writable and executable before the boot's
    /// audit of the kernel's tables, which must stop the boot on it. Judged by
    /// `kernel_wx_leaf_is_refused` and `virt_kernel_wx_leaf_is_refused`.
    kernel_wx_leaf = "kernel-wx-leaf";

    /// Run the first task to start off the bottom of its kernel stack. Judged,
    /// with the four below, by `a_kernel_stack_overflow_names_its_stack`.
    stack_overflow_task = "stack-overflow-task";

    /// Run the first CPU to idle off the bottom of its idle stack.
    stack_overflow_idle = "stack-overflow-idle";

    /// Send one NMI from the idle loop and run its handler off the bottom of
    /// its IST stack.
    stack_overflow_nmi = "stack-overflow-nmi";

    /// Run the boot CPU off the bottom of the boot stack once boot phases are
    /// done.
    stack_overflow_boot = "stack-overflow-boot";

    /// Run the `#DF` report of `stack-overflow-idle` off the bottom of IST1,
    /// the stack it is written on.
    // The double fault is x86-64's alone, so AArch64 builds an accessor it never reads.
    #[allow(dead_code)]
    stack_overflow_report = "stack-overflow-report";

    /// Read the TSC 10 ms ahead in the clock's samples once it has held for
    /// a second, and 10 ms further once that step is slewed away, as steps
    /// `IA32_TSC_ADJUST` would not show: the first is slewed, the second
    /// demotes the TSC. Judged by `a_stepped_tsc_is_slewed_then_demoted`.
    tsc_step = "tsc-step";

    /// Read the HPET 10 ms behind in its samples once the clock has held for
    /// a second, as an HPET that stood still while the TSC and the ACPI PM
    /// timer counted: the span is left out and the boot goes on. Judged by
    /// `a_stalled_hpet_is_left_out`.
    hpet_stall = "hpet-stall";

    /// Hand every CPU the loader's Intel update with its header's revision
    /// raised by one and its checksum sealed again: the read-back's negative
    /// control on real Intel, whose payload still says the revision it did.
    // An x86 patch's alone, so AArch64 builds an accessor it never reads.
    #[allow(dead_code)]
    microcode_reseal = "microcode-reseal";

    /// Hand every CPU the loader's AMD patch with the first byte of its data
    /// flipped: the negative control on the guarded write, whose `#GP` each
    /// CPU must report as a refusal and boot on.
    // An x86 patch's alone, so AArch64 builds an accessor it never reads.
    #[allow(dead_code)]
    microcode_amd_flip = "microcode-amd-flip";

    /// Skip the startup for the AP that would be cpu2, so a non-last AP never starts.
    smp_skip_ap = "smp-skip-ap";

    /// The roster's last two CPUs take the power-off's SGI and halt without
    /// `CPU_OFF`, so PSCI answers them on for the whole budget. Judged by
    /// `virt_off_names_the_cpus_left_on`.
    // PSCI is AArch64's alone, so x86-64 builds an accessor it never reads.
    #[allow(dead_code)]
    power_off_spares_the_last_two = "power-off-spares-the-last-two";

    /// `psci::init` keeps no conduit, so this kernel has no reset: a machine
    /// without PSCI. Judged by `virt_reboot_refused_without_psci`.
    // PSCI is AArch64's alone, so x86-64 builds an accessor it never reads.
    #[allow(dead_code)]
    psci_withheld = "psci-withheld";

    /// `power::init_reset` keeps no reset register, so this kernel ends the
    /// machine by processor shutdown: a machine whose FADT names none.
    /// Judged by `a_panic_with_no_reset_register_shuts_down`.
    // The FADT's reset register is x86-64's alone, so AArch64 builds an accessor it never reads.
    #[allow(dead_code)]
    reset_withheld = "reset-withheld";

    /// Take an invalid opcode once the IDT is loaded and before the clock,
    /// and panic inside its report: the DOUBLE PANIC path with nothing
    /// calibrated. Judged by `a_double_panic_before_the_clock_resets`.
    // The fault's report is x86-64's alone, so AArch64 builds an accessor it never reads.
    #[allow(dead_code)]
    test_double_panic = "test-double-panic";

    /// Panic from an AP's idle loop once the machine is released, and panic
    /// again inside that panic's first hold of the console registers, while
    /// every other CPU still runs. Judged by
    /// `a_panic_inside_the_console_hold_stops_the_others`.
    panic_in_the_hold = "panic-in-the-hold";

    /// Give the `iommu-testdev` off bus 0 no route, and once the SMMUv3 is
    /// armed have it and the one on bus 0 write on no domain, then the one on
    /// bus 0 on a domain at an address it maps, and there again once it is
    /// taken back. Judged by `virt_smmu`.
    // The SMMUv3 is AArch64's alone, so x86-64 builds an accessor it never reads.
    #[allow(dead_code)]
    smmu_selftest = "smmu-selftest";

    /// Time the same read loop on every CPU, either side of the `mov cr0` that enables caching.
    control_regs_bench = "control-regs-bench";

    /// Issue a fixed count of machine-wide TLB shootdowns against every CPU the
    /// machine brought up, with nothing else running, and report the distribution.
    tlb_shootdown_bench = "tlb-shootdown-bench";

    /// Panic once boot phases are done, with no thread current.
    test_late_panic = "test-late-panic";

    /// Panic a few seconds after a compositor claims the framebuffer, from an idle CPU.
    metal_panic_probe = "metal-panic-probe";

    /// Cap how long an idle CPU may sleep so the idle loop keeps running.
    diag_tick = "diag-tick";



    /// Shorten the panicked kernel's own reboot bound from a minute to seconds,
    /// so a guest reaches the reset. Judged by `screen_fatal_behind_a_painter`.
    panic_reboot_fast = "panic-reboot-fast";

    /// Have the last boot checkpoint's painter go fatal holding the panel's
    /// latch: a fatal path meeting a painter that will never let go.
    panel_painter_stalls = "panel-painter-stalls";
}

/// Recurse until the stack this runs on is spent: the `stack-overflow-*`
/// actuators' fault.
#[cfg(feature = "boot-actuators")]
#[inline(never)]
pub fn overflow_stack(depth: u64) -> u64 {
    let mut frame = [depth; 32];
    // Opaque, so the frame is kept and the call below is not a tail call.
    let frame = core::hint::black_box(&mut frame);
    if frame[0] == u64::MAX {
        return 0;
    }
    overflow_stack(depth + 1) + frame[31]
}

#[cfg(feature = "boot-actuators")]
const IMPLIES: &[(&str, &[&str])] = &[
    ("metal-panic-probe", &["diag-tick"]),
    ("stack-overflow-report", &["stack-overflow-idle"]),
];

#[cfg(feature = "boot-actuators")]
const ARM_WORDS: usize = NAMES.len().div_ceil(u64::BITS as usize);

#[cfg(feature = "boot-actuators")]
static ARMED: [AtomicU64; ARM_WORDS] = [const { AtomicU64::new(0) }; ARM_WORDS];

/// Arms what `cmdline` names; must run before any AP exists, so every later read is a race-free relaxed load.
#[cfg(feature = "boot-actuators")]
pub fn init(cmdline: &str) {
    let mut armed = [0u64; ARM_WORDS];
    for token in toyos_abi::boot::tokens(cmdline).filter(|t| !crate::params::claims(t)) {
        arm(&mut armed, token);
    }
    for (name, implied) in IMPLIES {
        if is_armed(&armed, name) {
            for one in *implied {
                arm(&mut armed, one);
            }
        }
    }
    // Published word by word, not atomically as a whole: sound because no reader exists yet.
    for (word, value) in ARMED.iter().zip(armed) {
        word.store(value, Ordering::Relaxed);
    }
    if armed.iter().any(|&word| word != 0) {
        log!("actuators: {cmdline}");
    }
}

#[cfg(feature = "boot-actuators")]
fn arm(armed: &mut [u64; ARM_WORDS], name: &str) {
    let (word, bit) = at(index_of(name));
    armed[word] |= bit;
}

#[cfg(feature = "boot-actuators")]
fn is_armed(armed: &[u64; ARM_WORDS], name: &str) -> bool {
    let (word, bit) = at(index_of(name));
    armed[word] & bit != 0
}

#[cfg(feature = "boot-actuators")]
const fn at(index: usize) -> (usize, u64) {
    (index / u64::BITS as usize, 1 << (index % u64::BITS as usize))
}

/// Refuses any actuator: a kernel with none must not boot looking like one that was given none.
#[cfg(not(feature = "boot-actuators"))]
pub fn init(cmdline: &str) {
    let mut named = toyos_abi::boot::tokens(cmdline).filter(|t| !crate::params::claims(t));
    assert!(
        named.next().is_none(),
        "this kernel carries no actuators and was handed the boot parameter {cmdline:?}"
    );
}

#[cfg(feature = "boot-actuators")]
fn index_of(name: &str) -> usize {
    let mut i = 0;
    while i < NAMES.len() {
        if NAMES[i] == name {
            return i;
        }
        i += 1;
    }
    panic!("boot parameter {name:?}: this kernel declares no such actuator");
}

// Const, not `index_of`+`at`: a typo in `$wire` fails the build instead of panicking at boot.
#[cfg(feature = "boot-actuators")]
const fn bit_of(name: &str) -> (usize, u64) {
    let mut i = 0;
    while i < NAMES.len() {
        if str_eq(NAMES[i], name) {
            return at(i);
        }
        i += 1;
    }
    panic!("undeclared actuator");
}

#[cfg(feature = "boot-actuators")]
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg(feature = "boot-actuators")]
const _: () = {
    assert!(
        ARM_WORDS * u64::BITS as usize >= NAMES.len(),
        "the arm set has fewer bits than there are actuators"
    );
    let mut i = 0;
    while i < NAMES.len() {
        let mut j = i + 1;
        while j < NAMES.len() {
            assert!(!str_eq(NAMES[i], NAMES[j]), "two actuators share a name");
            j += 1;
        }
        i += 1;
    }
    // Also validate every `IMPLIES` name, so a typo there fails the build rather than a boot.
    let mut i = 0;
    while i < IMPLIES.len() {
        let (name, implied) = IMPLIES[i];
        let _ = bit_of(name);
        let mut j = 0;
        while j < implied.len() {
            let _ = bit_of(implied[j]);
            j += 1;
        }
        i += 1;
    }
};
