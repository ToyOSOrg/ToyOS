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

    /// Have `iod` null SS, force a switch, and report whether it reloaded — the
    /// AMD `SYSRET` SS-attributes workaround's only guest-observable proof.
    sysret_ss_probe = "sysret-ss-probe";

    /// Log every i8042 drain: bytes seen, events queued, whether the queue woke.
    i8042_trace = "i8042-trace";

    /// Hold a scheduler pass between reading `irq_ring` and the byte ring so an interrupt lands between them.
    i8042_edge_race = "i8042-edge-race";

    /// Make the ISR's output-buffer check trip its 16-byte bound and run the quarantine path.
    i8042_fault = "i8042-fault";

    /// Zero the i8042 probe's init budget so its expiry paths run.
    i8042_budget_expired = "i8042-budget-expired";

    /// Hand the i8042 probe a FADT denying a controller that is present.
    i8042_fadt_denial = "i8042-fadt-denial";

    /// Answer the i8042 probe's scancode-set query with ECHO's own `0xEE`.
    i8042_kbd_echo = "i8042-kbd-echo";

    /// Shorten the i8042 post-verdict counter report from 10s to 500ms.
    i8042_fast_health = "i8042-fast-health";

    /// Cap the i8042 ISR at 4 bytes and answer empty until the mute verdict is out; `service` then polls the rest, so the verdict beats the sequence on every boot instead of on a loaded shard's luck.
    i8042_split_burst = "i8042-split-burst";

    /// Shorten the idle loop's health/PMM snapshot cadence from 10s to 200ms.
    sched_fast_health = "sched-fast-health";

    /// Script the input core directly at end of boot.
    test_input_merge = "test-input-merge";

    /// Clamp the xHCI driver to one device block.
    xhci_one_slot = "xhci-one-slot";

    /// Run the xHCI extended-capability walk over eight malformed lists at init.
    xhci_xecp_selftest = "xhci-xecp-selftest";

    /// Read and write every USB disk carrying the gate's stamp in block 0; the stamp, not the parameter, picks the disk, since the boot stick shares the bus.
    usb_storage_gate = "usb-storage-gate";

    /// Ask the NVMe disk for a block with the caller's operation budget already spent.
    nvme_spent_budget = "nvme-spent-budget";

    /// Refuse the first two FAT-1 mirror writes of a write-back drain flush; two, not one, because the retry ladder parks only at attempt 2.
    fat_mirror_write_refuse = "fat-mirror-write-refuse";

    /// Establish three nested `scheduler::Operation`s and report what each observed and restored; it stages nothing, touching no device.
    sched_operation_nesting = "sched-operation-nesting";

    /// Answer SYNCHRONIZE CACHE with ILLEGAL REQUEST / INVALID COMMAND OPERATION CODE.
    usb_flush_unimplemented = "usb-flush-unimplemented";

    /// The same, with a HARDWARE ERROR in place of ILLEGAL REQUEST.
    usb_flush_fails = "usb-flush-fails";

    /// Abandon the boot's first WRITE(10) data phase without waiting for it.
    usb_transport_break = "usb-transport-break";

    /// Skip the waits of the next Reset Recovery's control transfers, once.
    usb_reset_break = "usb-reset-break";

    /// Run `SYS_FSYNC`'s first attempt under an operation that is already over.
    fsync_budget_spent = "fsync-budget-spent";

    /// Make `SYS_FSYNC`'s deadman already expired.
    fsync_deadman_now = "fsync-deadman-now";

    /// Skip one NVMe completion wait so a submitted command goes unanswered.
    nvme_command_silent = "nvme-command-silent";

    /// Under-deliver one READ(10) data phase so the byte counts disagree.
    usb_short_read = "usb-short-read";

    /// Hold every mass-storage bulk completion back 2ms before the driver may see it.
    usb_slow_device = "usb-slow-device";

    /// Report the preempt depth and backtrace at the deepest point of a disk transfer; it stages nothing, only measures.
    io_depth_probe = "io-depth-probe";

    /// Park `iod` before it drains so a closed file's write-back stays pending.
    writeback_stall = "writeback-stall";

    /// Starve the four xHCI bring-up register waits in `init_one`.
    xhci_deaf_controller = "xhci-deaf-controller";

    /// Starve the port-reset wait in `init_device`.
    xhci_deaf_port = "xhci-deaf-port";

    /// Make one CPU ignore a kick.
    dump_deaf_cpu = "dump-deaf-cpu";

    /// Storm the CPU spinning on `syscall` from Ring 3 with NMIs.
    syscall_window_nmi = "syscall-window-nmi";

    /// Take the IST index off vector 2's gate — the negative control on the row above: the CPU builds the NMI frame at whatever `rsp` holds and takes a `#DF`.
    nmi_without_ist = "nmi-without-ist";

    /// Return from the NMI handler via `iretq` with a second NMI already pending.
    nmi_nested = "nmi-nested";

    /// Report an empty root hub for the first 300ms of boot.
    xhci_slow_connect = "xhci-slow-connect";

    /// Report the first root-hub port empty for the same window, the rest normal — distinct from hiding the whole bus, since settle waits only for a non-empty settled set.
    xhci_slow_storage_connect = "xhci-slow-storage-connect";

    /// Give PORTSC's PED bit the RW1CS meaning xHCI 1.2 §5.4.8 gives it.
    xhci_portsc_rw1c = "xhci-portsc-rw1c";

    /// Take a bound HID device's first completion away and hand back a stall.
    xhci_hid_break_first = "xhci-hid-break-first";

    /// The same at its fourth completion.
    xhci_hid_break_late = "xhci-hid-break-late";

    /// Run `parse_config` over nine crafted configuration descriptors at init.
    xhci_descriptor_selftest = "xhci-descriptor-selftest";

    /// Run `Virtqueue::poll_used` over eleven crafted used-ring elements at init.
    virtio_used_selftest = "virtio-used-selftest";

    /// Walk the PCI capability list, window check and parse over thirteen crafted config-space layouts at init.
    pci_cap_selftest = "pci-cap-selftest";

    /// Raise the local APIC's spurious vector on this CPU once.
    lapic_spurious_selftest = "lapic-spurious-selftest";

    /// Leave every AP holding the CR0/CR4 that INIT left it.
    no_ap_control_regs = "no-ap-control-regs";

    /// Skip the startup IPI for the AP that would be cpu2, so a non-last AP never starts.
    smp_skip_ap = "smp-skip-ap";

    /// Time the same read loop on every CPU, either side of the `mov cr0` that enables caching.
    control_regs_bench = "control-regs-bench";

    /// Shrink both disk caches to 64 entries each.
    test_small_caches = "test-small-caches";

    /// Shrink each process's VA arena from ~1015GB to 256MiB.
    test_tiny_va = "test-tiny-va";

    /// Panic once boot phases are done, with no thread current.
    test_late_panic = "test-late-panic";

    /// Take a Ring 0 `#UD` once boot phases are done, with no thread current.
    test_kernel_fault = "test-kernel-fault";

    /// Panic inside the crash report before it has said anything; armed alone, a boot that reports no crash never reaches it.
    panic_in_report = "panic-in-report";

    /// Take a `#PF` inside the crash report before it has said anything; armed alone, a boot that reports no crash never reaches it.
    fault_in_report = "fault-in-report";

    /// Panic a few seconds after a compositor claims the framebuffer, from an idle CPU.
    metal_panic_probe = "metal-panic-probe";

    /// Cap how long an idle CPU may sleep so the idle loop keeps running.
    diag_tick = "diag-tick";

    /// Log the monotonic time and which CPUs are alive every 250ms.
    heartbeat = "heartbeat";

    /// Have every CPU emit patterned log records at once from spawned kernel threads.
    log_storm = "log-storm";

    /// Remove the IF/TF bracket around shard selection through publication — the negative control on the log's interrupt-atomicity claim.
    log_unbracketed_reserve = "log-unbracketed-reserve";

    /// Send this CPU an IPI mid record-copy and emit one shard generation from the handler.
    log_nested_emit = "log-nested-emit";

    /// The same IPI, sent between the shard-pointer read and the unlocked `xadd` — stages order damage the log gate detects, unlike the row above's invisible corruption.
    log_nested_reserve = "log-nested-reserve";

    /// Turn the reservation's `xadd` into a load, an open interrupt window, and a store.
    log_shared_reservation = "log-shared-reservation";

    /// Let a handle close cancel every poll on `Source::Log` in the machine.
    log_close_cancels_any_syscap = "log-close-cancels-any-syscap";

    /// Let a handle close cancel every poll on `Source::Keyboard` in the machine.
    keyboard_close_cancels_every_console = "keyboard-close-cancels-every-console";

    /// Bypass `ConsoleObject`'s line buffer so writes interleave as they arrive.
    console_unbuffered = "console-unbuffered";

    /// Panic inside `klogd` on its first instruction.
    klogd_panic = "klogd-panic";

    /// Panic inside `usbd` on its first instruction.
    usbd_panic = "usbd-panic";

    /// Stop the boot dead in phase 3, interrupts off, before any log drain.
    pre_idle_wedge = "pre-idle-wedge";

    /// Fail every re-read of a page of a file through `FatBacking`.
    fat_backing_read_fails = "fat-backing-read-fails";

    /// Fail every filesystem read of the mounted boot volume, at the `Fat32` layer rather than `FatBacking`'s page read.
    fat_boot_reads_fail = "fat-boot-reads-fail";

    /// Leave the NVMe controller out of the IOMMU's root table.
    iommu_context_absent = "iommu-context-absent";

    /// Give it a present context entry naming an empty second-level table, distinct from an absent context: passthrough would fault identically to the row above.
    iommu_empty_domain = "iommu-empty-domain";

    /// Run the HDA register allow-list over every arm of it at bind time.
    hda_allowlist_selftest = "hda-allowlist-selftest";

    /// Make the wall clock's update flag never clear.
    rtc_dead = "rtc-dead";

    /// Make the RTC registers never settle: no two of four reads agree.
    rtc_unstable = "rtc-unstable";

    /// Make firmware name no century register.
    rtc_no_century = "rtc-no-century";

    /// Make the century register read `0x21`.
    rtc_century_next = "rtc-century-next";

    /// Make firmware name its own timezone.
    rtc_zone_east = "rtc-zone-east";

    /// Run the leak-rollback controls (device mint, FAT reopen) after mount.
    leak_rollback_selftest = "leak-rollback-selftest";
}

#[cfg(feature = "boot-actuators")]
const IMPLIES: &[(&str, &[&str])] = &[
    ("i8042-trace", &["i8042-fast-health", "i8042-edge-race"]),
    ("usb-short-read", &["usb-storage-gate"]),
    ("metal-panic-probe", &["diag-tick"]),
    ("heartbeat", &["diag-tick"]),
    ("syscall-window-nmi", &["diag-tick"]),
];

#[cfg(feature = "boot-actuators")]
const ARM_WORDS: usize = NAMES.len().div_ceil(u64::BITS as usize);

#[cfg(feature = "boot-actuators")]
static ARMED: [AtomicU64; ARM_WORDS] = [const { AtomicU64::new(0) }; ARM_WORDS];

/// Arms what `cmdline` names; must run before any AP exists, so every later read is a race-free relaxed load.
#[cfg(feature = "boot-actuators")]
pub fn init(cmdline: &str) {
    let mut armed = [0u64; ARM_WORDS];
    for token in cmdline.split(',').filter(|t| !t.is_empty()) {
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

/// Refuses any parameter: a kernel with no actuators must not boot looking like one that was given none.
#[cfg(not(feature = "boot-actuators"))]
pub fn init(cmdline: &str) {
    assert!(
        cmdline.is_empty(),
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
