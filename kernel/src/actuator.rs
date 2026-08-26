//! What this kernel has been told to break, and nothing a boot decides.
//!
//! There are two kernels: the one an image ships, which carries none of this,
//! and the one the suite boots, which carries all of it and arms whichever the
//! boot parameter names.
//!
//! **The shipping kernel is not this kernel with the switches off.** Without
//! `boot-actuators` every accessor below is `const fn … { false }`, so a call
//! site folds to the shipped branch and a constant an actuator moves stays a
//! constant. There is no arm-set, no name, and no parser: `init` refuses a
//! parameter rather than ignoring one, because a shipping kernel handed a test
//! parameter must not boot pretending it was given nothing.
//!
//! The parameter arrives in [`toyos_abi::boot::KernelArgs`], written to
//! `\toyos\cmdline` on the ESP by `src/image.rs` and passed on by the
//! bootloader. That is what makes it readable before the first actuator fires:
//! `test-early-panic` panics between arming the on-screen console and
//! `mm::init`, and `no-ap-control-regs` acts at AP bring-up, so nothing the
//! kernel could mount or ask for later is early enough.
//!
//! It is our own bootloader's string and crosses no trust boundary, so an
//! unknown token is a bug in this build system and panics by name.

#[cfg(feature = "boot-actuators")]
use core::sync::atomic::{AtomicU64, Ordering};

/// Every declared actuator: the accessor the kernel calls, the name a boot
/// parameter uses, and what nothing else can reach.
///
/// **The comment on each says what it stages and, in one clause, why that state
/// cannot be staged from the host side** — the claim that earns it a place, and
/// what separates an actuator from a harness that could have injected the same
/// thing from outside.
macro_rules! actuators {
    ($( $(#[$doc:meta])* $name:ident = $wire:literal; )*) => {
        /// In bit order: an actuator's bit is its index here.
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
            // **The one allow this file needs, and it is the shipping arm.**
            // Many of these accessors have their only call site inside a module
            // that is itself `#[cfg(feature = "boot-actuators")]`, so in a
            // shipping kernel the constant is compiled and the caller is not.
            // Deleting them would make the claim that every actuator folds to a
            // constant hold per-actuator instead of whole.
            #[allow(dead_code)]
            #[inline(always)]
            pub const fn $name() -> bool {
                false
            }
        )*
    };
}

actuators! {
    /// Panic between arming the on-screen console and `mm::init`, so a test can
    /// assert the console covers the window where nothing else can report.
    test_early_panic = "test-early-panic";

    /// One log line per i8042 drain: bytes seen, events queued, whether the
    /// queue was woken — the only way a test can assert the wake guard from
    /// outside. Implies `i8042-fast-health` and `i8042-edge-race`: this group is
    /// the only boot that counts interrupts against bytes.
    i8042_trace = "i8042-trace";

    /// Hold a scheduler pass between reading the source's `irq_ring` record and
    /// reading its byte ring, so an interrupt lands in between and the pass sees
    /// bytes it has been told nothing about. Unwidened that window is a handful
    /// of instructions on one CPU, which nothing host-side can time.
    i8042_edge_race = "i8042-edge-race";

    /// Make the ISR's output-buffer check lie, so its 16-byte bound trips and
    /// the quarantine path runs — a controller that chatters forever is the one
    /// case the bound alone still lets livelock a CPU.
    i8042_fault = "i8042-fault";

    /// Set the i8042 probe's whole init budget to zero, so its expiry paths run
    /// on a controller that is answering perfectly: QEMU answers every step in
    /// microseconds and no real EC has ever been timed.
    i8042_budget_expired = "i8042-budget-expired";

    /// Hand the i8042 probe a FADT denying the controller on a machine that has
    /// a working one. QEMU cannot stage that disagreement: the bit is read from
    /// the same QOM tree the device lives in, so there the claim and the
    /// hardware always agree.
    i8042_fadt_denial = "i8042-fadt-denial";

    /// Answer the i8042 probe's `0xF0 0x00` scancode-set query with ECHO's own
    /// `0xEE` on a keyboard that is otherwise working — QEMU's PS/2 keyboard
    /// implements `0xF0` to the letter and no device or machine property makes
    /// it stop. Only the verdict is replaced; both bytes still go out.
    i8042_kbd_echo = "i8042-kbd-echo";

    /// Shorten the i8042's post-verdict counter report from 10 s to 500 ms. Its
    /// claim is about what happens over tens of seconds and no guest test
    /// program lives that long; only the period moves.
    i8042_fast_health = "i8042-fast-health";

    /// Shorten the idle loop's own health/PMM snapshot cadence
    /// (`scheduler.rs`'s `snapshot_interval_ns`) from 10 s to 200 ms. Telling a
    /// spinning CPU from a halting one needs two prints to compare and no guest
    /// test program lives past the shipped 10 s once; only the period moves.
    /// See `i8042_quarantine`'s idle-trip check,
    /// `issues/kernel/i8042-quarantine-health-line-count-is-vacuous.md`.
    sched_fast_health = "sched-fast-health";

    /// Script the input core directly at end of boot. QEMU activates one input
    /// handler per device class, so two keyboards or two pointers can never both
    /// be live in a guest.
    test_input_merge = "test-input-merge";

    /// Clamp the xHCI driver to one device block, so a test can drive the path
    /// where the controller hands back a slot the DMA pool has no room for.
    /// QEMU's `nec-usb-xhci,slots=N` does not reach HCSPARAMS1 and its Enable
    /// Slot ignores the MaxSlotsEn a driver writes.
    xhci_one_slot = "xhci-one-slot";

    /// Run the xHCI extended-capability walk over eight malformed lists at init.
    /// The list comes from firmware and QEMU's controller publishes none at all,
    /// so the bounds in `xhci/legacy.rs` would otherwise ship never having
    /// executed.
    xhci_xecp_selftest = "xhci-xecp-selftest";

    /// Read and write every USB disk that carries the gate's stamp in block 0,
    /// at the end of the peripheral phase. A raw block device has no path to
    /// userland, so the kernel is the only in-guest actor that can drive one;
    /// the stamp and not the parameter decides which disk gets written, because
    /// the boot stick is on the same bus. See `kernel/src/usb_gate.rs`.
    usb_storage_gate = "usb-storage-gate";

    /// Ask the NVMe disk for a block with the caller's operation budget already
    /// spent, once, under both of the page cache's locks — where every real
    /// caller asks from. A disk slow enough to spend `block::OPERATION` cannot be
    /// staged: QEMU's NVMe answers in microseconds and `rerror`/`werror` fail a
    /// command rather than delaying one. Armed by `cache_eviction`; see
    /// `kernel/src/nvme_gate.rs`.
    nvme_spent_budget = "nvme-spent-budget";

    /// Refuse the first **two** FAT-1 (mirror) writes of a **write-back drain**
    /// flush on the log volume, as a budget expiry — the mid-mirror refusal that
    /// leaves the active FAT durable and the mirror not. Nothing host-side
    /// reaches it: `rerror`/`werror` fail the whole drive rather than one write
    /// of a pair. Two, not one, because the drain's retry ladder parks only at
    /// attempt 2, which is where a standing `WORK` arm would be armed twice
    /// (`writeback::drain_all_iod`). See `kernel/src/fat32_adapter.rs`.
    fat_mirror_write_refuse = "fat-mirror-write-refuse";

    /// Establish three nested `scheduler::Operation`s with known deadlines and
    /// report what every level observed and what each drop restored, once from a
    /// boot phase and once from `iod`'s body. The type reaches `percpu::cpu_id`
    /// and `driver::current_handle` and `kernel/` is excluded from the host
    /// workspace, so nothing off a booted machine can construct one. It stages
    /// nothing: no device is touched and every guard is dropped before the
    /// function returns. See `kernel/src/sched_gate.rs`.
    sched_operation_nesting = "sched-operation-nesting";

    /// Answer SYNCHRONIZE CACHE with the ILLEGAL REQUEST / INVALID COMMAND
    /// OPERATION CODE a stick with no write cache gives. QEMU's `scsi-disk`
    /// implements 0x35 for every front end that reaches it, so a device that
    /// *cannot* flush is unreachable from the host side. The command is still
    /// issued; only its verdict is replaced. See `xhci/wait/msc.rs`'s
    /// `flush_sense`.
    usb_flush_unimplemented = "usb-flush-unimplemented";

    /// The same, with a HARDWARE ERROR: a device that cannot flush rather than
    /// one that will not.
    usb_flush_fails = "usb-flush-fails";

    /// Abandon the data phase of the boot's first WRITE(10) without waiting for
    /// it — the state a transfer that ran out `USB_TIMEOUT_NS` leaves behind.
    /// QEMU's `usb-storage` answers every CBW, data phase and CSW in order, and
    /// `rerror`/`werror` fail the whole drive instead of leaving a transfer in
    /// flight. The TRB is really on the ring and the controller really completes
    /// it; only the wait is skipped. See `xhci/wait/msc.rs`'s `transport_break`.
    usb_transport_break = "usb-transport-break";

    /// Skip the waits of the next Reset Recovery's control transfers — the
    /// Bulk-Only Mass Storage Reset and both CLEAR_FEATUREs — once, which is the
    /// state a device that stopped answering EP0 leaves behind. QEMU's
    /// `usb-storage` answers every EP0 request in microseconds with no property
    /// to stop it. The requests are really enqueued and the doorbell really
    /// rung; only the waits are skipped. See `xhci/wait/msc.rs`'s `reset_break`.
    usb_reset_break = "usb-reset-break";

    /// Run every `SYS_FSYNC`'s first attempt under an operation that is already
    /// over, so the flush sequence is refused on the caller's budget at the
    /// shipped site and the operation-level retry loop's keep-the-volume half
    /// runs. `usb-slow-device`'s 2 ms against a 2 s bound is three orders of
    /// magnitude short and no QEMU option stages the rest. Every attempt after
    /// the first runs clean. See `object/ops.rs`'s `fsync`.
    fsync_budget_spent = "fsync-budget-spent";

    /// Make `SYS_FSYNC`'s deadman already expired, so the first budget-refused
    /// attempt is the last and the declared-failed exit runs. The real deadman is
    /// minutes by design, which no staged boot can wait out honestly; only *when*
    /// it expires is replaced. See `object/ops.rs`'s `fsync`.
    fsync_deadman_now = "fsync-deadman-now";

    /// Skip one NVMe completion wait, so a submitted command goes unanswered
    /// against a live controller and the reset escalation runs. QEMU's NVMe
    /// answers every command in microseconds and `rerror`/`werror` fail one
    /// rather than delaying it, so silence inside `nvme::COMMAND` is unreachable
    /// from the host side. The command really is submitted, the doorbell really
    /// rung and the completion really owed. See `kernel/src/nvme_gate.rs`.
    nvme_command_silent = "nvme-command-silent";

    /// Under-deliver one READ(10) data phase where the gate asks for it, so the
    /// controller's count of the bytes it moved and the device's own CSW residue
    /// disagree. QEMU derives both from one transfer, so they can never
    /// contradict each other there. The bytes left in the tail of the window are
    /// the previous transfer's, read off that window rather than invented. See
    /// `xhci/wait/msc.rs`'s `short_read`.
    usb_short_read = "usb-short-read";

    /// Hold every mass-storage bulk completion back for 2 ms before the driver
    /// may see it, which is what a USB flash stick's erase block does to a 4 KiB
    /// write. QEMU's `usb-storage` answers in microseconds and has no property
    /// that delays a transfer. The event is the controller's own and the bytes
    /// really moved; only when the driver may see it is replaced. See
    /// `xhci/mod.rs`'s `SLOW_TRANSFER_NS`.
    usb_slow_device = "usb-slow-device";

    /// Report the preempt depth at the deepest point of a disk transfer, with
    /// the backtrace that got there — how much of the kernel is holding a
    /// spinlock while a device is being waited for, which no static read of the
    /// call graph produces. The instrument the work in
    /// `issues/kernel/every-wait-in-this-kernel-is-a-spin.md` is judged on. It
    /// measures and stages nothing.
    io_depth_probe = "io-depth-probe";

    /// Park `iod` before it drains, so the write-back a closed file owes stays
    /// pending while a test re-opens the file. Which thread drains the queue and
    /// when is decided inside the guest, and no QEMU device or machine property
    /// delays a kernel thread. `SYS_SHUTDOWN`'s own drain is not on this thread,
    /// so a stalled boot still shuts down. See `kernel/src/iod.rs`.
    writeback_stall = "writeback-stall";

    /// Starve the four xHCI bring-up register waits in `init_one` on a
    /// controller that is otherwise answering. QEMU's xHC halts, resets, clears
    /// CNR and starts in microseconds; no device or machine property makes a
    /// register bit not settle. See `xhci/mod.rs`'s `settles`.
    xhci_deaf_controller = "xhci-deaf-controller";

    /// The same for the port-reset wait in `init_device`, which QEMU finishes
    /// synchronously and where unplugging between the port scan and the reset is
    /// not expressible either.
    xhci_deaf_port = "xhci-deaf-port";

    /// Make one CPU ignore a kick — the machine state the blocked-task dump
    /// exists to describe, and one QEMU cannot stage because it delivers every
    /// IPI it is given. The victim really clears IF and really spins, for longer
    /// than the dump's kick budget and then no longer. See `sched/dump.rs`'s
    /// `deaf_window`.
    dump_deaf_cpu = "dump-deaf-cpu";

    /// Storm the CPU that is spinning on `syscall` from Ring 3 with NMIs, and
    /// count how many arrive at CPL 0 with a user `rsp` — the window
    /// `arch::idt`'s IST2 row exists for. QEMU has no way to aim an IPI at an
    /// instruction, so no host-side stimulus puts an NMI in a three-instruction
    /// window. The storm arms on the victim's own syscall count and never on a
    /// clock (`nmi_gate::SPINNING_SYSCALLS`), so it cannot fire at a machine
    /// where nothing is spinning yet. See `kernel/src/nmi_gate.rs`.
    syscall_window_nmi = "syscall-window-nmi";

    /// Take the IST index off vector 2's gate, leaving the handler, the ring and
    /// everything else exactly as it ships — the negative control on the row
    /// above, where the CPU builds the NMI frame at whatever `rsp` holds, SMAP
    /// refuses the user page, and the machine takes a `#DF`. The IDT is the
    /// guest's own memory and no QEMU property edits it.
    nmi_without_ist = "nmi-without-ist";

    /// Return from inside the NMI handler through an `iretq` with a second NMI
    /// already pending, which is the one way a second NMI can enter on the stack
    /// the first is standing on. It is one CPU, inside a handler, between two
    /// instructions, so no host-side stimulus can produce it. See
    /// `kernel/src/nmi_gate.rs`'s `stage_nested_if_armed`.
    nmi_nested = "nmi-nested";

    /// Report an empty root hub for the first 300 ms of the boot, so the port
    /// scan runs against a controller that has not finished detecting its
    /// devices yet — what every physical root hub does after HCRST and what
    /// QEMU's cannot, because it answers PORTSC from the QOM tree. The register
    /// is replaced, not a verdict. See `xhci/mod.rs`'s `SLOW_CONNECT_NS`.
    xhci_slow_connect = "xhci-slow-connect";

    /// Report the *first* root-hub port empty for the same window, leaving every
    /// other port on the machine reading normally. Not a weaker version of the
    /// row above: `await_connect_settle` ends as soon as the connect set has held
    /// still and is non-empty, so a bus whose other devices have settled settles
    /// on them and hiding the whole bus can never reach it. See `xhci/mod.rs`'s
    /// `SLOW_STORAGE_PORT`.
    xhci_slow_storage_connect = "xhci-slow-storage-connect";

    /// Give PORTSC's PED bit (bit 1) the RW1CS meaning xHCI 1.2 §5.4.8 gives it:
    /// a port software writes a '1' to goes from Enabled to Disabled and reads
    /// PED clear until it is reset again. QEMU's `xhci_port_write` has PED in
    /// neither the set it clears on a written '1' nor its read/write set. The
    /// register is replaced, not a verdict. See
    /// `XhciController::software_disabled`.
    xhci_portsc_rw1c = "xhci-portsc-rw1c";

    /// Take a bound HID device's very first completion away and hand the driver
    /// a stall in its place, which is the shape a hot-plugged mouse showed on
    /// real hardware. QEMU's `usb-hid` has no path to `USB_RET_STALL` for an
    /// interrupt IN on endpoint 1. The report that transfer delivered is taken
    /// away with the code, so a driver that dispatched it anyway cannot publish
    /// a delta it never earned. See `xhci/hid.rs`'s `stage_break`.
    xhci_hid_break_first = "xhci-hid-break-first";

    /// The same at its fourth, where a device that has been delivering stops.
    xhci_hid_break_late = "xhci-hid-break-late";

    /// Run `parse_config` over nine crafted configuration descriptors at init.
    /// The bytes come from a device and every device QEMU can attach describes
    /// itself correctly, so the parser's refusals would otherwise ship never
    /// having executed. See `xhci/device.rs`'s `selftest`.
    xhci_descriptor_selftest = "xhci-descriptor-selftest";

    /// Run `Virtqueue::poll_used` over eleven crafted used-ring elements at
    /// init. Both fields are device-written and no device property, machine
    /// property or backend makes one report a head descriptor it was never given
    /// or a length past its buffer, so the parse's refusals would otherwise ship
    /// never having executed. The queue and its DMA page are real and the
    /// shipped `poll_used` is what runs; only the writer of the ring is the
    /// kernel. See `drivers/virtio.rs`'s `used_selftest`.
    virtio_used_selftest = "virtio-used-selftest";

    /// Leave every AP holding the CR0 and CR4 that INIT left it: caching
    /// disabled, WP clear, NE clear. A control register is written by the guest
    /// and read by nobody outside it, so no QEMU device, machine property or
    /// `-cpu` flag can leave one wrong. Only the two register writes are absent,
    /// so the CPU the per-CPU line names is a real divergent CPU.
    no_ap_control_regs = "no-ap-control-regs";

    /// Time the same read loop on every CPU, either side of the `mov cr0` that
    /// turns its caching on. The answer is not on this host — TCG models no cache
    /// at all, so `CR0.CD` there costs nothing. See `arch/control_regs.rs`'s
    /// `bench`.
    control_regs_bench = "control-regs-bench";

    /// Shrink both disk caches to 64 entries each. The honest ceilings are tens
    /// of megabytes, so a test that reached them by doing real I/O would spend
    /// minutes proving what 256 KiB proves in a second. Only the bound moves.
    test_small_caches = "test-small-caches";

    /// Shrink each process's VA arena from ~1015 GB to 256 MiB, so that
    /// exhausting it is a test rather than a physics problem: every region costs
    /// at worst twice its size in physical memory, so the shipped arena needs
    /// upwards of 500 GB of RAM to fill and the PMM refuses long first. See
    /// `vma::alloc_floor`.
    test_tiny_va = "test-tiny-va";

    /// Panic once the boot phases are done, with no thread current, so a test
    /// can drive the ordinary fatal panic — the one path where the report
    /// reaches the screen only because the panic handler captured it before
    /// draining the ring.
    test_late_panic = "test-late-panic";

    /// Take a Ring 0 `#UD` once the boot phases are done, with no thread
    /// current, so `fatal_exception` runs its `Blame::Kernel` arm. Every
    /// exception this suite can otherwise stage is a *program*'s and ends at
    /// `recover_or_halt`'s process arm; no QEMU device, machine property or
    /// guest program puts a Ring 0 frame on a bad instruction, so that arm and
    /// the `DOUBLE PANIC` branch under it have no other caller.
    test_kernel_fault = "test-kernel-fault";

    /// Panic inside the crash report, before it has said anything: at the head
    /// of `crash_report_panic` and at the head of `fatal_exception`, which are
    /// the two reports this kernel writes. It is a second failure *inside* the
    /// handler for the first, on one CPU, between two statements, so no
    /// injection reaches there. Armed alone it changes nothing: a boot that
    /// reports no crash reaches neither site. See
    /// `issues/panic-path/a-double-panic-at-boots-edge-says-nothing-but-its-name.md`.
    panic_in_report = "panic-in-report";

    /// Take a `#PF` inside the crash report, at the head of
    /// `crash_report_panic`, before it has said anything. `panic-in-report`'s
    /// neighbour with the other kind of second failure, and the one
    /// `fatal_exception`'s recursive short-circuit exists for: a wild read
    /// between two statements of the handler for the first crash, on one CPU,
    /// which no injection from outside reaches. Armed alone it changes nothing.
    fault_in_report = "fault-in-report";

    /// Panic a few seconds after a *compositor* has claimed the framebuffer,
    /// from an idle CPU, so the panic handler's recovery branch is not taken and
    /// the report reaches `halt_all_cpus`. What nothing else reaches: whether a
    /// fatal report lands on the owner's panel while his desktop owns the
    /// scanout — QEMU's framebuffer is host RAM, the laptop's a write-combining
    /// MMIO mapping the compositor is also writing. Implies `diag-tick`: the
    /// probe's deadline is read from the idle loop.
    metal_panic_probe = "metal-panic-probe";

    /// Cap how long a CPU with nothing to run may sleep, so the idle loop keeps
    /// running on a machine that has gone quiet. The shipping kernel is right
    /// not to — a pass that found no work and no deadline stops the LAPIC timer
    /// and halts, which is good power management — but everything the kernel
    /// does *for the person watching it* lives in that loop. It costs the wakes
    /// and the flushes they carry, so the instrument is no longer a passive
    /// observer of the boot it is watching.
    diag_tick = "diag-tick";

    /// One log line every 250 ms carrying the monotonic time and which CPUs are
    /// still alive. What nothing else reaches: *was this boot alive at time T* —
    /// a live idle desktop and a dead machine have the same last thing to say,
    /// so the log otherwise records a freeze's existence and never its time.
    /// Implies `diag-tick`, without which a clear bit means "this CPU had
    /// nothing to do" rather than "this CPU is gone".
    heartbeat = "heartbeat";

    /// Every CPU emitting patterned log records at once, from kernel threads the
    /// boot's first `SYS_LOG_READ` spawns. A real workload's record rate is a
    /// handful of lines a second and cannot be made to saturate a shard however
    /// long a test waits. The records go through the shipped `emit`, reservation
    /// and publication; only their number and their text belong to the test. See
    /// `kernel/src/log/storm.rs`.
    log_storm = "log-storm";

    /// Remove the IF/TF bracket `arch::LogCommitGuard` holds from shard
    /// selection through final publication, leaving a guard that masks nothing —
    /// the negative control on the log's whole interrupt-atomicity claim, with
    /// the type constructed and dropped exactly as it ships. Nothing on the host
    /// can stage it: there is no injection that interrupts a kernel between two
    /// instructions. See `arch::LogCommitGuard::close`, whose header carries the
    /// migration half this kernel cannot reach.
    log_unbracketed_reserve = "log-unbracketed-reserve";

    /// Send this CPU its own IPI from halfway through a log record's body copy,
    /// and have the handler emit exactly one shard generation of patterned
    /// records — an interrupt that logs, inside another `emit`, on one CPU. Loom
    /// models threads and not CPU flags, so the claim that a nested writer
    /// cannot collide with the writer it interrupted has no model, and nothing
    /// on the host can interrupt a kernel between two instructions of one
    /// function. See `kernel/src/log/nested.rs`.
    log_nested_emit = "log-nested-emit";

    /// The same IPI, sent from **between the shard-pointer read and the
    /// unlocked `xadd`** instead — the first of the two windows the commit
    /// guard's bracket closes. The row above stages a corruption no reader can
    /// see; this one stages the damage to the shard's *order*, which `read.rs`'s
    /// `Descent::advance` is written against and `test-runner`'s log gate
    /// refuses. Nothing on the host reaches it, for the row above's reason. See
    /// `kernel/src/log/nested.rs`'s `reserve_window`.
    log_nested_reserve = "log-nested-reserve";

    /// Turn the reservation's one unlocked `xadd` into a load, an open interrupt
    /// window and a store — the shape that is *not* atomic against an interrupt
    /// on its own CPU. The window is what makes it deterministic rather than a
    /// race: on one CPU the only thing that can be made to come between the load
    /// and the store is an interrupt this kernel sent itself. Nothing else
    /// changes. See `arch::percpu_fetch_add`.
    log_shared_reservation = "log-shared-reservation";

    /// Let a handle close cancel every poll in the machine on `Source::Log` —
    /// which is every `SysCap`'s, so any process closing any capability posts
    /// `-NotFound` into logd's parked poll. Which process closes which handle is
    /// decided inside the guest and the two need not know about each other at
    /// all, which is both the shape of the bug and why the host cannot stage it.
    /// The question the kernel asks is the *source*'s
    /// (`Source::ended_by_its_last_handle`), so this restores exactly the log
    /// half.
    log_close_cancels_any_syscap = "log-close-cancels-any-syscap";

    /// Let a handle close cancel every poll in the machine on
    /// `Source::Keyboard` — the keyboard claim's, and every `Console`'s: the one
    /// process holding the claim closing its handle posts `-NotFound` into every
    /// pending `POLL_ADD` on stdin, which libc's terminal read is what arms. The
    /// keyboard half of the row above, unstageable from the host for the same
    /// reason. `Source::ended_by_its_last_handle`, in `kernel/src/inbox.rs`.
    keyboard_close_cancels_every_console = "keyboard-close-cancels-every-console";

    /// Bypass `ConsoleObject`'s line buffer: every userland `write` reaches the
    /// backend as it arrives, so a line one process began another finishes.
    /// `println!` is `LineWriter`, whose two syscalls per line are decided inside
    /// the guest's own `std`, and no host-side stimulus makes the kernel forget a
    /// buffer it holds. `console_line_atomicity` counts the interleavings and
    /// `Serial::interleaved` names the kernel-into-userland half.
    console_unbuffered = "console-unbuffered";

    /// Panic inside `klogd`, the kernel thread, on its first instruction.
    /// Nothing outside the kernel can make a kernel thread panic — there is no
    /// process to kill and no syscall to make — and the verdict is not the panic
    /// but which *branch* the panic handler takes.
    klogd_panic = "klogd-panic";

    /// Panic inside `usbd`, the second kernel thread, on its first instruction.
    /// `klogd-panic`'s other half: the two carry opposite rows in
    /// `sched::kthread`, so this one's recovery runs through `poison_tid`, the
    /// idle loop's `reap_poisoned` and `zombify_poisoned`, none of which
    /// otherwise sees a task with no address space of its own.
    usbd_panic = "usbd-panic";

    /// Stop the boot dead in phase 3, with interrupts off, before anything that
    /// could ever have drained a log. There is no injection that stops a kernel
    /// between two statements, and a QEMU pause stops the guest without leaving
    /// it in the state under test — a CPU that will never reach a scheduler pass.
    /// Without `Drain::Inline` a boot that stops here produces nothing at all,
    /// because the machine's only other two drains are the timer tick and the
    /// idle loop.
    pre_idle_wedge = "pre-idle-wedge";

    /// Fail every re-read of a page of a file on either FAT mount through
    /// `FatBacking`, with the mount and the filesystem underneath it working.
    /// Both partitions are on the disk the guest runs from, so no QEMU-side way
    /// of failing its reads leaves the machine booted and the volumes mounted.
    /// The read is still issued and only its verdict is replaced. See
    /// `fat32_adapter.rs`'s `fat_backing_reads`.
    fat_backing_read_fails = "fat-backing-read-fails";

    /// Fail every *filesystem* read of the boot volume once it is mounted, with
    /// the mount and the rest of the machine working. Its sibling above injects
    /// one layer higher, at `FatBacking::read_page`, which reaches no directory
    /// entry; this one is under `Fat32` itself, where a directory entry, a FAT
    /// chain and an extent list are read. `Role::Boot` because nothing in the
    /// kernel reads that volume after the mount, so a process can still be sent
    /// to ask it a question. See `fat32_adapter.rs`'s `boot_volume_reads`.
    fat_boot_reads_fail = "fat-boot-reads-fail";

    /// Leave the NVMe controller out of the IOMMU's root table, so it reaches
    /// translation with no context entry. A root table, a context entry and a
    /// page table are all the kernel's own memory, so no QEMU device or machine
    /// property reaches that state while leaving the rest of the machine
    /// correct. The transaction really happens, the unit really blocks it, and
    /// the fault record is the hardware's own. See `iommu/vtd/mod.rs`'s `enable`.
    iommu_context_absent = "iommu-context-absent";

    /// Give it a present context entry naming a second-level table with no
    /// mappings in it. Not a weaker first: this one alone separates a real
    /// second-level walk from a context entry naming passthrough, which would
    /// fault identically for the row above.
    iommu_empty_domain = "iommu-empty-domain";

    /// Run the HDA register allow-list over every arm of it at bind time, and
    /// report each verdict by name. The check is gated on holding the device
    /// claim and the claim is exclusive, so a guest test could only reach the
    /// allow-list on a machine with no audio. The shipped path runs; only the
    /// caller is staged. See `kernel/src/drivers/hda.rs`'s `allowlist_selftest`.
    hda_allowlist_selftest = "hda-allowlist-selftest";

    /// A wall clock whose update flag never clears. QEMU has no switch that
    /// removes or wedges the mc146818 and its RTC always presents the guest a
    /// coherent register set. What is replaced is what the *hardware* answers;
    /// the decoder and everything downstream of it are shipped code. See
    /// `kernel/src/rtc.rs`.
    rtc_dead = "rtc-dead";

    /// One whose registers never settle: no two of four reads agree.
    rtc_unstable = "rtc-unstable";

    /// Firmware that names no century register. The FADT a guest reads is
    /// generated by QEMU and always names it at 0x32.
    /// See `drivers/acpi.rs`'s `rtc_century_register`.
    rtc_no_century = "rtc-no-century";

    /// A century register reading 0x21. `-rtc base=` sets every digit of the
    /// date except the century: a guest booted at 2101 reads century 20 and
    /// year 01.
    rtc_century_next = "rtc-century-next";

    /// A machine whose firmware names its zone. OVMF ships
    /// `EFI_UNSPECIFIED_TIMEZONE` and nothing in QEMU sets the UEFI variable that
    /// would change it, so without this every emulated boot assumes UTC and the
    /// arithmetic between local time and UTC never runs. See `clock::init_wall`.
    rtc_zone_east = "rtc-zone-east";
}

/// What arming one arms with it.
#[cfg(feature = "boot-actuators")]
const IMPLIES: &[(&str, &[&str])] = &[
    ("i8042-trace", &["i8042-fast-health", "i8042-edge-race"]),
    ("usb-short-read", &["usb-storage-gate"]),
    ("metal-panic-probe", &["diag-tick"]),
    ("heartbeat", &["diag-tick"]),
    ("syscall-window-nmi", &["diag-tick"]),
];

/// Words the arm set takes, from how many names there are.
///
/// Derived rather than written down, so the next name past a multiple of 64
/// costs nothing and no second number has to be kept agreeing with [`NAMES`].
/// The `const` block at the foot of this file refuses a set with fewer bits than
/// there are actuators, so overflowing it is a compile error and never a boot
/// that quietly reads somebody else's bit.
#[cfg(feature = "boot-actuators")]
const ARM_WORDS: usize = NAMES.len().div_ceil(u64::BITS as usize);

#[cfg(feature = "boot-actuators")]
static ARMED: [AtomicU64; ARM_WORDS] = [const { AtomicU64::new(0) }; ARM_WORDS];

/// Arm what the boot parameter names, before anything can read it.
///
/// Called from `kernel_main` before the earliest actuator site and before any
/// AP exists, so every later read is a plain relaxed load of a word this CPU
/// wrote or a word a CPU that did not yet exist will find already written.
///
/// **The set is built whole and published word by word, and that is sound for
/// the same reason** — there is no reader yet, so a torn publication is not a
/// state anything can observe.
///
/// A token this kernel does not declare panics by name. It came from our own
/// image builder through our own bootloader, so it is a bug in this build
/// system and not input — there is no trust boundary anywhere on that path.
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
    for (word, value) in ARMED.iter().zip(armed) {
        word.store(value, Ordering::Relaxed);
    }
    if armed.iter().any(|&word| word != 0) {
        log!("actuators: {cmdline}");
    }
}

/// Set `name`'s bit in a set being built.
#[cfg(feature = "boot-actuators")]
fn arm(armed: &mut [u64; ARM_WORDS], name: &str) {
    let (word, bit) = at(index_of(name));
    armed[word] |= bit;
}

/// Is `name`'s bit set in a set being built? [`IMPLIES`] asks; nothing else
/// does, because every other reader is an accessor with its bit resolved at
/// compile time.
#[cfg(feature = "boot-actuators")]
fn is_armed(armed: &[u64; ARM_WORDS], name: &str) -> bool {
    let (word, bit) = at(index_of(name));
    armed[word] & bit != 0
}

/// An index in [`NAMES`] as the word it lives in and the bit inside that word.
/// One expression, so the accessor's `const` and the two runtime callers cannot
/// disagree about where a name's bit is.
#[cfg(feature = "boot-actuators")]
const fn at(index: usize) -> (usize, u64) {
    (index / u64::BITS as usize, 1 << (index % u64::BITS as usize))
}

/// A kernel with no actuators in it refuses a parameter rather than ignoring
/// one: a boot that was asked for a machine this binary cannot be must not come
/// up looking like the machine that was asked for.
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

/// The word and bit `name` is armed in, resolved where a typo is a compile error
/// rather than a boot that quietly reads somebody else's actuator.
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
    // Every side of [`IMPLIES`] too, so a name it misspells is this build
    // failing rather than the one boot that asks for it panicking.
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
