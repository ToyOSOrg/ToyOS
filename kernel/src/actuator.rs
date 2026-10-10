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
    /// Wedge the machine at the shutdown syscall — after the job list has run
    /// and before anything is torn down — instead of resetting. The negative
    /// control on `crate::deadline`: nothing else in this kernel ends it.
    wedge_before_reset = "wedge-before-reset";

    /// Panic between arming the on-screen console and `mm::init`.
    test_early_panic = "test-early-panic";

    /// Take an undefined-instruction exception right after the architecture's
    /// console step, where it has one to take there: the earliest fault the
    /// exception vectors must report. AArch64 installs its vectors in the entry;
    /// x86-64 loads its IDT later, and panics by name instead.
    test_early_fault = "test-early-fault";

    /// Have the first syscall null SS, force a switch, and report whether it reloaded — the
    /// AMD `SYSRET` SS-attributes workaround's only guest-observable proof.
    sysret_ss_probe = "sysret-ss-probe";

    /// Leave the i8042 unprobed, so this kernel drives no controller and an
    /// `isa` claim on it is granted. Judged by `isa_ports_are_the_binders_alone`
    /// and `isa_lines_reach_their_holder`.
    i8042_withheld = "i8042-withheld";

    /// Script the input core directly at end of boot.
    test_input_merge = "test-input-merge";

    /// Run the xHCI extended-capability walk over eight malformed lists at init.
    xhci_xecp_selftest = "xhci-xecp-selftest";

    /// Establish three nested `scheduler::Operation`s and report what each observed and restored; it stages nothing, touching no device.
    sched_operation_nesting = "sched-operation-nesting";

    /// Abandon the boot's first WRITE(10) data phase without waiting for it.
    usb_transport_break = "usb-transport-break";

    /// Sweep the boot stick from the shutdown syscall so the reset lands on a
    /// controller that is moving bytes rather than on a bus idle since the
    /// wedge. See `usb_gate::sweep_under_load`; judged by
    /// `usb_reset_records_the_phase_it_cut`.
    usb_reset_under_load = "usb-reset-under-load";

    /// Give the machine's stop no time: one sweep, and a thread not parked at
    /// it outlasts the stop. Judged by `machine_shutdown_short_stop`.
    stop_budget_spent = "stop-budget-spent";

    /// Make one CPU ignore a kick.
    dump_deaf_cpu = "dump-deaf-cpu";

    /// Wedge one CPU with interrupts off, spinning on a lock another CPU holds
    /// and never gives back: the negative control on `crate::hardlockup`, and a
    /// machine nothing else in this tree ends. Where CPUID states no
    /// performance counter it also has one CPU send the victim the NMI the
    /// counter would have, which is the only way a TCG guest reaches that
    /// decision; on hardware the counter does it and nothing is sent.
    hard_lockup_probe = "hard-lockup-probe";

    /// Send one NMI from the idle loop, and return from its handler via `iretq` with a second NMI already pending.
    nmi_nested = "nmi-nested";

    /// Run `parse_config` over nine crafted configuration descriptors at init.
    xhci_descriptor_selftest = "xhci-descriptor-selftest";

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

    /// Withhold `VIRTIO_F_ACCESS_PLATFORM` from every virtio device but the console, staging a function whose addresses the unit never translates.
    virtio_no_access_platform = "virtio-no-access-platform";

    /// Leave every IOMMU unit queueing, translating and remapping through
    /// tables of its own, as firmware may hand one over, just before this
    /// kernel programs it. Judged by `iommu_firmware_left`.
    iommu_firmware_left = "iommu-firmware-left";

    /// Read the DMAR's flags with `INTR_REMAP` clear, as on a platform whose
    /// firmware says it does not remap. Judged by `claim_refused_without_remapping`.
    iommu_no_remap = "iommu-no-remap";

    /// Leave every AP holding the CR0/CR4 that INIT left it.
    no_ap_control_regs = "no-ap-control-regs";

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

    /// Seal the black box under an identity that is not this stick's, which is
    /// what a record another image left in the same memory looks like. The
    /// loader pass after it must clear the record and boot its kernel, not
    /// report it and end the chain.
    blackbox_foreign_identity = "blackbox-foreign-identity";

    /// Panic a few seconds after a compositor claims the framebuffer, from an idle CPU.
    metal_panic_probe = "metal-panic-probe";

    /// Cap how long an idle CPU may sleep so the idle loop keeps running.
    diag_tick = "diag-tick";

    /// Run the leak-rollback controls (device mint) after mount.
    leak_rollback_selftest = "leak-rollback-selftest";

    /// Run the revoked-backing controls after mount.
    revoked_backing_selftest = "revoked-backing-selftest";

    /// Shorten the panicked kernel's own reboot bound from a minute to seconds,
    /// so a guest reaches the reset. Judged by `screen_fatal_behind_a_painter`.
    panic_reboot_fast = "panic-reboot-fast";

    /// Have Ctrl+Alt+D's report painter go fatal holding the panel's latch: a
    /// fatal path meeting a painter that will never let go.
    panel_painter_stalls = "panel-painter-stalls";
}

#[cfg(feature = "boot-actuators")]
const IMPLIES: &[(&str, &[&str])] = &[
    ("metal-panic-probe", &["diag-tick"]),
    // The staged CPU has to still be deaf when its bound passes, and this boot
    // would otherwise have handed the machine back at the end of its job list —
    // so the control that ends a machine no other bound ends is staged over the
    // one that stops this machine ending itself.
    ("hard-lockup-probe", &["wedge-before-reset"]),
];

#[cfg(feature = "boot-actuators")]
const ARM_WORDS: usize = NAMES.len().div_ceil(u64::BITS as usize);

#[cfg(feature = "boot-actuators")]
static ARMED: [AtomicU64; ARM_WORDS] = [const { AtomicU64::new(0) }; ARM_WORDS];

/// Arms what `cmdline` names; must run before any AP exists, so every later read is a race-free relaxed load.
#[cfg(feature = "boot-actuators")]
pub fn init(cmdline: &str) {
    let mut armed = [0u64; ARM_WORDS];
    for token in toyos_abi::boot::actuators(cmdline).filter(|t| !crate::params::claims(t)) {
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
    let mut named = toyos_abi::boot::actuators(cmdline).filter(|t| !crate::params::claims(t));
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
