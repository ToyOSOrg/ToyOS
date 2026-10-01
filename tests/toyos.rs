mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use common::qemu::{
    self, await_guest, await_marker, BootOptions, QemuInstance,
    STALLED, TIMED_OUT,
};
use common::{audio, compile, devices, faults, lan, metal, power, screen, serial, usb};
use toyos_build::bootlog::{self};
use toyos_build::testargs::{self, Shard, SUITE};
use toyos_build::redlist;

/// Whether a test may run while other guests are up.
///
/// Every entry of [`MACHINE_TESTS`] and [`SCREEN_TESTS`] answers this or does
/// not compile. That is the serial-by-default rule
/// in its stronger form: the rule's whole safety argument is that *forgetting*
/// must cost a slow suite rather than a wrong measurement, and a name that
/// cannot be added without an answer cannot be forgotten at all.
///
/// **Where the answer is not known it is [`Sched::Serial`].** A wrong `Parallel`
/// is a test measuring a machine it does not have to itself, and neither the
/// suite nor the agent reading its red can tell that from a real defect.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sched {
    /// May run beside other guests. Its assertions hold on a host running as
    /// many QEMUs as the width allows.
    Parallel,
    /// Runs in the serial tail: one guest on the host, after the parallel phase
    /// has drained. Everything with a wall-clock margin on either clock, a
    /// debounce window staged from the host, or a rate.
    Serial,
}

/// The width with no `--jobs`, and where it came from.
///
/// 14 cores and about three host threads a guest divides out to four. The suite
/// says twelve. Alternated in one session on a quiet host, 246 tests, both
/// green: **125.6 s wide eight against 109.1 s wide twelve**, with the parallel
/// phase at 58.3 s against 42.1 s — the same 16 s and the same direction as the
/// pair taken on the tree six commits earlier. A guest here is mostly
/// *waiting* — for a marker, for a debounce, for a device — which is why this is
/// a measurement and not a division.
///
/// **Twelve is the number for one suite on this host.**
/// An earlier table said eight; it was taken while `drain_serial` was still
/// width-scaled and
/// `metal_sim_pointer_churn`'s twenty-four paced drains *were* the phase.
const DEFAULT_WIDTH: usize = 12;

/// The shared-boot binaries that call `SYS_DEBUG`, and so cannot run on the
/// kernel an image ships.
///
/// **Everything else on the shared boot now runs on that kernel** — no features,
/// the same staged artifact `cargo run --build-only` writes. Until
/// `test-actuators` became one name, `qemu::fold_inert` put it on *every* test
/// kernel, so nothing in this suite had ever booted the shipping binary and two
/// of the three names below depended on that without saying so.
///
/// A second boot rather than a second image: the block costs a fraction of a
/// second of guest time between its members, and what these need is a syscall
/// number the other 150 must not have.
const ACTUATOR_TESTS: &[&str] = &[
    // Actions 10 and 11: the address of sixteen bytes of kernel memory and
    // whether they still hold what the kernel put there. A guest cannot read the
    // kernel's address space, so without them a kernel that still made the write
    // under test answers a userland that cannot notice.
    "abuse_kernel_addr",
    // Action 16, the live-object census per kind, and 17 and 18 for the idle
    // stack the deferred release path runs on. A leak is two readings and a
    // comparison, so on a kernel that answers `InvalidArgument` both readings
    // are the same error and the assertion passes having counted nothing.
    "handle_basic",
    "handle_kill_policy",
    "handle_transfer",
    // The last two took action 16 in place of `SYS_SYSINFO`: a verdict about
    // what one killed process gave back cannot be the whole machine's free
    // memory, which every other binary in a shared boot moves under it.
    "handle_lifetime",
    "shm_release_reclaims",
];

/// What [`ACTUATOR_TESTS`] boots: the one kernel that carries `SYS_DEBUG`, with
/// no actuator armed in it.
const ACTUATOR_KERNEL: &[&str] = toyos_build::build::TEST_KERNEL;

// Rust helper binaries that are spawned by tests, not tests themselves.
const RUST_SKIP: &[&str] = &[
    // **Its exit code is a measurement, not a verdict**, and the shared block
    // judges every member on `exit=0` alone — so it would red on every boot
    // that measured anything. It also needs the real-time band, which only
    // `tests/latencycase` endows. `latency_wake` runs it there.
    "cyclictest",
    // Its verdict is a ratio of cycle counts, which a guest's host moves: the
    // `wake_storm_cost` metal row runs it on the T14.
    "wake_storm_cost",
    // Its product is a cycle count per syscall and the clock rate beside it,
    // which a guest's host sets: the `syscall_cost` metal row runs it.
    "syscall_cost",
    // Its verdict is a duration: whether a shootdown waits for every other CPU
    // is read off the clock around the syscall. The `tlb_shootdown_waits`
    // metal row runs it on the T14.
    "tlb_shootdown_waits",
    // The C corpus's comparator: a helper reached through one symlink per case,
    // never a test of its own. `shared_metal` stages every name on this list.
    "ccheck",
    "disk_backtrace_child",
    "fault_gate_child",
    // It takes the machine down; `virt_fatal_halts_the_others_first` runs it.
    "panic_halts_first",
    // It asserts nothing at all: it holds a `tests/lancase` boot open for
    // twenty seconds so the host can reach this machine over the cable. On a
    // shared boot it would be twenty seconds of nothing.
    "lan_hold",
    // The same for `tests/lantalkcase`, held until the runner's bound is near
    // unless the host's `reboot` over ssh ends it first. `lan_talk` rides it.
    "lan_talk_hold",
    // The swapping boot's hold: it lasts until the host's `reboot` over ssh,
    // and the runner's bound is the fallback. `lan_swap` rides it.
    "lan_swap_hold",
    // Fills /tmp to the VFS listing limit, so it needs a boot nothing else
    // shares — every later `read_dir("/tmp")` in it would be refused.
    // `readdir_bound` gives it one.
    "readdir_bound",
    // Fills the VFS `created_dirs` cap and leaves it there. `mkdir_cap` runs it.
    "mkdir_cap",
    // Its failure mode is a CPU that never runs anything again, so on the
    // shared boot it would be reported against whichever test came next — and
    // every one after that. `short_sleep_livelock` gives it a boot of its own.
    "abuse_short_sleep",
    // Audio is judged on the T14 and nowhere else: the `hda_client_stall`,
    // `hda_tone`, `audio_idle_suspend`, `shipped_client_departures` and
    // `soundd_log_stall` metal rows run these.
    "hda_client_stall",
    "audio_tone",
    "audio_idle_suspend",
    "null_sink_client_exits",
    "soundd_log_stall",
];

/// Binaries a metal row or a guest test drives that the shared boot also runs
/// on purpose.
///
/// **A binary driven under a different name is still discovered by
/// [`discover_rust_tests`]**, still runs on the shared boot, and there passes on
/// its exit code with nothing staged for it to act on. `RUST_SKIP` is one
/// answer to that; this list is the other, for the binaries whose shared run
/// asserts something of its own. Every driven name is on one list or the
/// other, so neither answer is silence — `suite_split` is the gate.
#[allow(dead_code, reason = "`suite_split` reads it, in `toyos-checks` alone")]
const DRIVEN_AND_SHARED: &[&str] = &[
    // Its shared run is the x86-64 verdict; `virt_readonly_copyout` builds it
    // for AArch64 and runs it on that architecture's job case.
    "abuse_readonly_copyout",
    // The lost-wake canary: its shared run is the count on the shipping
    // kernel with nothing staged, and `blocking_read_window` drives it again
    // with the watch's window held open.
    "blocking_read_stress",
    "sched_stress",
    "std_alloc",
];

/// What `test-early-panic` panics with (`kernel/src/main.rs`): the last line its
/// report puts on serial.
const EARLY_PANIC_MESSAGE: &str = "test-early-panic: on-screen console check";

// Tests that read a decoded screendump, which is exactly the set for which
// the screen is the device under test: the panel. The T14 has no serial port
// and the metal loop cannot read its glass, so what reaches the panel is
// asserted here and nowhere else. The AArch64 rows boot the `virt` machine,
// the only one that architecture has.
const SCREEN_TESTS: &[(&str, Sched, qemu::Profile)] = &[
    // The log is still on the panel once every program the image starts has
    // run: an event, and no clock in it.
    ("screen_diag_boot", Sched::Parallel, qemu::Profile::Metal),
    ("screen_console_shell", Sched::Parallel, qemu::Profile::Metal),
    ("screen_panic_muted", Sched::Parallel, qemu::Profile::Metal),
    // The same fatal path from inside Ctrl+Alt+D's report painter, holding the
    // panel's latch it will never give back: the report has to take the screen
    // anyway, and its CPU has to go on to watch the reset bound.
    ("screen_fatal_behind_a_painter", Sched::Parallel, qemu::Profile::Gop),
    // The same fatal path with a compositor holding the panel, which is the
    // only configuration the owner's laptop is ever in.
    ("screen_fatal_halt_composited", Sched::Parallel, qemu::Profile::Metal),
    ("virt_early_panic", Sched::Parallel, qemu::Profile::Virt),
    ("virt_early_fault", Sched::Parallel, qemu::Profile::Virt),
    ("virt_el2_drop", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_user_mode", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_timer_preempts", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_irq_storm", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_timer_floor", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_fp_isolation", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_first_entry", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_unmap_touch", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_debug_refused", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_readonly_copyout", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_smp", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_el1_smp", Sched::Parallel, qemu::Profile::VirtTcg),
    ("virt_failed_ap_leaves_no_hole", Sched::Parallel, qemu::Profile::VirtEl2),
    ("virt_fatal_halts_the_others_first", Sched::Parallel, qemu::Profile::VirtEl2),
];

/// What `screen_console_shell` types, and what it then looks for on its own.
///
/// The command's *output* differs from the command, which is the whole point:
/// the shell echoes what is typed, so an assertion satisfiable by the echo says
/// only that the console drew a key, not that anything ran. This is asserted as
/// a whole trimmed row, so the echoed `/home/toy> echo zqjxk` cannot satisfy
/// it either.
const CONSOLE_NONCE: &str = "zqjxk";
/// `/system/bin/shell` cds to `$HOME` before its first prompt, and prints
/// `"{cwd}> "` — without the trailing space, which the decoder trims off the
/// end of every row.
const CONSOLE_PROMPT: &str = "/home/toy>";
/// The seed's witness on the panel.
///
/// `/system/bin/console` draws the boot so far, as `logd` serves it, above its
/// first prompt, so a panel carrying one of its lines is a console that read
/// it. This one is written hundreds of lines into a boot, which is what makes
/// its *absence* two different things — see `screen_console_shell`.
const CONSOLE_SEED_WITNESS: &str = "i8042:";

/// A program's line on the console, under the name of the pipe it came out of:
/// init's, said just before it starts the console, so it is in the boot the
/// console is handed.
const CONSOLE_PROGRAM_WITNESS: &str = "init} init: started console";

/// The test whose machine shape *is* the test, on a boot of its own.
/// `run_machine_test` dispatches it.
const MACHINE_TESTS: &[(&str, Sched)] = &[
    // The owner's freeze, staged: `device_del` on the stick carrying `/boot`
    // and `/log` while the desktop draws. Serial because both verdicts are
    // liveness ceilings — two 2 s compositor reporting intervals inside 20 s,
    // and a console round trip inside 20 s — and a guest sharing the host with
    // eleven others answers those late for reasons that are not the defect.
    ("usb_boot_stick_pulled", Sched::Serial),
];

/// **The metal profile**: which registrations run on the ThinkPad T14, what
/// boots each one needs, and how each is judged off the log the stick came back
/// with.
///
/// Two rows naming the same boot share one image and one boot, and their job
/// lists are unioned. A boot is about a minute of the machine's time, so that
/// grouping is what the suite's cost is; the boot is *named* by an arm rather
/// than derived from its config and parameters, because sharing is not always
/// safe and only the author knows.
const METAL: &[(&str, metal::Metal)] = &[
    (
        // The device list: the T14's own xHCI, stick, i8042, HDA, framebuffer
        // and NVMe, asserted from the records the shipping kernel writes.
        "metal_device_probe",
        metal::Metal { arms: METALDEVICECASE, judge: |b| devices::on_metal(b[0]) },
    ),
    (
        "lan_dhcp_lease",
        metal::Metal { arms: LANCASE, judge: |b| lan::on_metal(b[0]) },
    ),
    (
        // Folded into `lan_dhcp_lease`'s judge once the PHY is brought up
        // (#453): lancase's own first-message record then carries this fact.
        "lan_message_delivery",
        metal::Metal { arms: LANICSCASE, judge: |b| lan::provoked_on_metal(b[0]) },
    ),
    (
        // The first byte: a lease from the bench's own router, read off the
        // stick, while the host pings the address this machine had before.
        "lan_lease_report",
        metal::Metal { arms: LANLEASECASE, judge: |b| lan::leased_on_metal(b[0]) },
    ),
    (
        "lan_talk",
        metal::Metal { arms: LANTALKCASE, judge: |b| lan::talked_on_metal(b[0]) },
    ),
    (
        // netd swapped for the build's own binary while the boot runs, by a
        // second `toyos-metal --swap` beside the flashing one; the stick's
        // `/log` is the oracle that nothing rebooted between the two netds.
        "lan_swap",
        metal::Metal { arms: LANSWAPCASE, judge: |b| common::swap::swapped_on_metal(b[0]) },
    ),
    // ---- one image: tests/testcases, no parameters, one job list ----
    (
        "blackbox_unclaimed_page",
        metal::Metal { arms: TESTCASES, judge: |b| power::blackbox_unclaimed(&b[0].loader(), &b[0].kernel()) },
    ),
    (
        // The machine's own CPU count, off the SMP bring-up records — a source
        // independent of the `control_regs:` lines it is then held to. The QEMU
        // registration says four because the harness staged four; here the
        // laptop says how many it has.
        "control_regs",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| control_regs(b[0].kernel().text(), b[0].cpus()?),
        },
    ),
    (
        "ioapic_topology",
        metal::Metal { arms: TESTCASES, judge: |b| ioapic_topology(b[0].kernel().text()) },
    ),
    (
        "klogd_hosted",
        metal::Metal { arms: TESTCASES, judge: |b| klogd_hosted(&b[0].kernel()) },
    ),
    (
        // The stimulus a host types at a console in QEMU is this boot's own job
        // list on the T14: every job that runs and exits is a process exit, and
        // the census is printed at each one.
        "irq_census_conservation",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| irq_census(b[0].kernel().text()),
        },
    ),
    (
        // `log-close` is a runner builtin and prints its `survived=` evidence to
        // a console nothing is on. What crosses is the child it spawns only
        // after the poll survived the close — and the boot reaching its last job
        // at all, which a builtin returning non-zero prevents.
        "log_poll_outlives_a_close",
        metal::Metal { arms: TESTCASES, judge: |b| log_close_survived(b[0]) },
    ),
    (
        "mkdir_cap",
        metal::Metal {
            arms: TESTCASES_MKDIR,
            judge: |b| b[0].job_passed("test_rs_mkdir_cap"),
        },
    ),
    (
        "readdir_bound",
        metal::Metal {
            arms: TESTCASES_READDIR,
            judge: |b| b[0].job_passed("test_rs_readdir_bound"),
        },
    ),
    (
        "short_sleep_livelock",
        metal::Metal {
            arms: TESTCASES,
            // A livelocked CPU produces no exit record at all, which is the
            // whole verdict: the defect this is aimed at was caught twice by NMI
            // on this very machine.
            judge: |b| b[0].job_passed("test_rs_abuse_short_sleep"),
        },
    ),
    (
        "wake_storm_cost",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| b[0].job_passed("test_rs_wake_storm_cost"),
        },
    ),
    (
        // The shipped tone client, twice in series, plays to completion and
        // exits 0, and soundd names how each left.
        "shipped_client_departures",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_null_sink_client_exits")?;
                audio::departures_on_metal(&b[0].log())
            },
        },
    ),
    (
        // soundd with no client costs no CPU, before any client has connected.
        "audio_idle_suspend",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_audio_idle_suspend")?;
                audio::idle_suspend_on_metal(&b[0].log())
            },
        },
    ),
    (
        "hda_tone",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_audio_tone")?;
                audio::tone_on_metal(&b[0].log())
            },
        },
    ),
    (
        "hda_client_stall",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_hda_client_stall")?;
                audio::client_stall_on_metal(&b[0].log())
            },
        },
    ),
    // ---- the audio boot of its own ----
    (
        "soundd_log_stall",
        metal::Metal {
            arms: LOGSTALLCASE,
            judge: |b| {
                b[0].job_passed("test_rs_soundd_log_stall")?;
                audio::log_stall_on_metal(&b[0].log())
            },
        },
    ),
    // ---- one image: tests/testcases armed with the chipset watchdog ----
    (
        // Two boots, and they cannot merge: the control is the same image
        // *without* the parameter, which is the boot every row above rides —
        // so this costs one image and not two, and its control is a machine
        // several other verdicts were already taken from.
        "loader_watchdog_arms",
        metal::Metal {
            arms: &[
                metal::once("testcases-watchdog", "tests/testcases", &["watchdog"], &[]),
                // The batch's job list is the union of its riders'; this arm
                // needs none of its own.
                metal::once("testcases", "tests/testcases", &[], &[]),
            ],
            judge: |b| {
                power::watchdog_armed(&b[0].loader(), &b[0].kernel())?;
                power::watchdog_quiet(&b[1].loader(), &b[1].kernel())
            },
        },
    ),
    (
        // The two numbers `syscall_cost` measures, printed by the job and read
        // off the stick: that it ran, and what it said, are the verdict.
        "syscall_cost",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_syscall_cost")?;
                b[0].log().must_say("syscall_cost: tsc ")?;
                b[0].log().must_say(" cycles/syscall over ")?;
                Ok(())
            },
        },
    ),
    (
        // `SYS_DEBUG` holds each other CPU's shootdown acknowledgement back,
        // so the kernel that carries it; the verdict is the guest's own exit.
        "tlb_shootdown_waits",
        metal::Metal {
            arms: &[metal::Arm {
                features: toyos_build::build::TEST_KERNEL,
                ..metal::once("testcases-debug", "tests/testcases", &[], &["test_rs_tlb_shootdown_waits"])
            }],
            judge: |b| b[0].job_passed("test_rs_tlb_shootdown_waits"),
        },
    ),
    (
        // The canary beside the `watch-window` actuator, and the count of
        // windows a post landed in while it ran — the staging that is
        // timing, and so metal's.
        "blocking_read_window",
        metal::Metal {
            arms: &[metal::once(
                "testcases-window",
                "tests/testcases",
                &["watch-window"],
                &["test_rs_blocking_read_stress"],
            )],
            judge: |b| {
                b[0].job_passed("test_rs_blocking_read_stress")?;
                window_held_on_metal(&b[0].kernel())
            },
        },
    ),
    (
        // One CPU deafened by the actuator, named by the blocked-task dump and
        // found by its NMI where it spins.
        "dump_nmi_probe",
        metal::Metal {
            // Held open by a job, because an empty list ends the boot before the
            // actuator arms.
            arms: &[metal::once(
                "testcases-deaf",
                "tests/testcases",
                &["dump-deaf-cpu"],
                &["test_rs_lan_hold"],
            )],
            judge: |b| faults::dump_nmi_probe_on_metal(&b[0].kernel()),
        },
    ),
    (
        // A fed watchdog: the armed boot runs its whole list to its own stop,
        // which a chipset reset anywhere in it would have cut short.
        "watchdog_fed",
        metal::Metal {
            arms: &[metal::once("testcases-watchdog", "tests/testcases", &["watchdog"], &[])],
            judge: |b| b[0].log_reached_the_stick(),
        },
    ),
    // ---- the boot facts, riding the same tests/testcases image ----
    //
    // **Six names and no extra minute of the machine.** Every needle below is a
    // record the *shipping* kernel writes on any boot it takes, so each rides
    // whatever image is already going on the stick; the two rows after them are
    // the two things this suite measures rather than reads, and those cost a
    // boot.
    (
        "smp_roster_and_tsc_trail",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| smp_roster_and_tsc_trail(b[0].kernel().text(), b[0].cpus()?),
        },
    ),
    (
        "pmm_accounting",
        metal::Metal { arms: TESTCASES, judge: |b| pmm_accounting(b[0].kernel().text()) },
    ),
    (
        "acpi_table_inventory",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| acpi_table_inventory(b[0].kernel().text()),
        },
    ),
    (
        "timer_calibration",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                timer_calibration(b[0].kernel().text())?;
                tsc_agrees_with_cpuid(b[0].kernel().text())
            },
        },
    ),
    (
        "pci_inventory",
        metal::Metal { arms: TESTCASES, judge: |b| pci_inventory(b[0].kernel().text()) },
    ),
    // ---- one image: tests/latencycase armed with the shootdown bench ----
    (
        "tlb_shootdown_cost",
        metal::Metal {
            arms: LATENCYCASE,
            // The machine's own CPU count, off the bring-up records rather than
            // off a number the harness staged: the QEMU registration says eight
            // because it asked for eight, and here the laptop says how many it
            // has.
            judge: |b| {
                let (p50, p99) = tlb_shootdown_cost(b[0].kernel().text(), b[0].cpus()?)?;
                b[0].measured("tlb.latencycase.p50_ns", p50)?;
                b[0].measured("tlb.latencycase.p99_ns", p99)
            },
        },
    ),
    (
        "latency_wake",
        metal::Metal {
            arms: LATENCYCASE,
            judge: |b| {
                b[0].job_passed("test_rs_sched_stress")?;
                wake_latency_recorded(b[0])
            },
        },
    ),
    // ---- one image: tests/jobcase ----
    (
        // Two boots the suite already flashes: the device boot writes and
        // fsyncs megabytes before its reset, and `jobcase` is the same reset
        // with nothing moved across the bus. Neither costs the machine a
        // minute it was not already spending.
        "usb_reset_hands_devices_back",
        metal::Metal {
            arms: USB_RESET_BOOTS,
            judge: power::usb_reset_on_metal,
        },
    ),
    (
        // Already precisely this boot: `args = ["reboot"]`. On the T14 the
        // chain is what every metal boot does — the loader points `BootNext` at
        // itself before each handoff, so the pass that reads the page appends
        // its report to the same `loader.log` the driver hands back.
        "blackbox_done_chain",
        metal::Metal { arms: JOBCASE, judge: |b| power::done_chain(&b[0].after_the_reset()?) },
    ),
    (
        // Its own boot, and it must not share one: it is the only arm in this
        // profile that deliberately leaves the machine unable to end its own
        // boot, and what it judges is that the machine ended it anyway.
        "boot_deadline_ends_a_wedge",
        metal::Metal {
            arms: &[metal::once("deadlinewedge", "tests/jobcase", &["wedge-before-reset"], &[])],
            judge: |b| power::deadline_wedge_chain(&b[0].after_the_reset()?),
        },
    ),
    (
        // One boot: a machine writing to the stick continuously, reset out from
        // under itself by the deadline with the controller mid-transfer, and
        // the stick enumerable on the next host afterwards.
        "usb_reset_records_the_phase_it_cut",
        metal::Metal {
            arms: &[metal::once("usbload", "tests/jobcase", &["usb-reset-under-load"], &[])],
            judge: |b| power::usb_load_chain(&b[0].after_the_reset()?),
        },
    ),
    (
        "usb_stick_left",
        metal::Metal {
            arms: &[metal::once("usbbreak", "tests/jobcase", &["usb-transport-break"], &[])],
            judge: |b| usb::transport_break_on_metal(&b[0].kernel(), &b[0].after_the_reset()?),
        },
    ),
    (
        // Its own boot, and the one arm in this profile the machine itself is
        // the instrument for: QEMU's TCG guest has no performance counter, so
        // only here is the NMI that samples a deaf CPU the counter's own. It
        // ends the machine at half the metal bound, with the whole second half
        // of the deadline still to run, so which record the page carries says
        // which of the two bounds ended it.
        "hard_lockup_ends_a_deaf_cpu",
        metal::Metal {
            arms: &[metal::once("hardlockup", "tests/jobcase", &["hard-lockup-probe"], &[])],
            judge: |b| power::hard_lockup_chain(&b[0].kernel(), &b[0].after_the_reset()?),
        },
    ),
    (
        // Its own boot, and it must not share one: it deliberately leaves the
        // page holding a record no stick owns, and a boot that then read it as
        // a predecessor's is exactly what the arm above judges.
        "blackbox_foreign_record",
        metal::Metal {
            arms: &[metal::once(
                "foreignrecord",
                "tests/jobcase",
                &[toyos_build::metal::FOREIGN_RECORD_ARM],
                &[],
            )],
            judge: |b| {
                let after = b[0].after_the_reset()?;
                let said = after.must_say(bootlog::FOREIGN_DONE)?.to_string();
                power::says_nothing_of(&after, bootlog::PREVIOUS_PANIC)?;
                power::says_nothing_of(&after, "the last boot read")?;
                // The hang `toyos-metal` admits for this arm, and only this one.
                after.must_say(bootlog::HUNG_WITHOUT_A_RECORD)?;
                eprintln!("  [power] {}", said.trim());
                Ok(())
            },
        },
    ),
    (
        // The machine came back to `sshd`, which is what tells a reset from the
        // S5 power-off the QEMU stop reason exists to catch — the driver
        // established it before this judge ran. What is left is the kernel's own
        // decode, and `0xcf9 <- 0x0f` is q35's register rather than this one's.
        "machine_reboot",
        metal::Metal {
            arms: JOBCASE,
            judge: |b| {
                power::reset_register_decoded(&b[0].kernel())?;
                bootlog::handed_back(b[0].after_the_reset()?.text()).map_err(|why| why.to_string())
            },
        },
    ),
    // ---- one image: tests/metalcase ----
    (
        "metal_sim_scanout_wc",
        metal::Metal { arms: METALCASE, judge: |b| scanout_wc(b[0].kernel().text()) },
    ),
    // ---- one image, eleven actuators, ten tests ----
    // The cheapest cluster there is: every one of these arms a check that runs
    // at init, logs its verdict and does nothing else, so they cost one flash
    // between them. **Nothing had to be promoted into `kernel/src/params.rs`**
    // — the metal profile flashes test images (the track's ruling), so an
    // actuator is armed the way the QEMU registration arms it and the
    // pre-flash gate is what says the machine survives each one.
    (
        "pci_capability_walk",
        metal::Metal { arms: SELFTESTS, judge: |b| pci_cap_selftest(b[0].kernel().text()) },
    ),
    (
        "process_reopen_selftest",
        metal::Metal { arms: SELFTESTS, judge: |b| process_reopen(b[0].kernel().text()) },
    ),
    (
        "read_fault_selftests",
        metal::Metal { arms: SELFTESTS, judge: |b| read_fault_probes(b[0].kernel().text()) },
    ),
    (
        "leak_rollback_selftest",
        metal::Metal { arms: SELFTESTS, judge: |b| leak_rollback(b[0].kernel().text()) },
    ),
    (
        "lapic_spurious_vector",
        metal::Metal { arms: SELFTESTS, judge: |b| lapic_vectors(b[0].kernel().text()) },
    ),
    (
        // The T14's own controller publishes a real capability list, which is
        // the half of this QEMU cannot give: q35's nec-usb-xhci has no USB
        // Legacy Support capability in it at all.
        "xhci_xecp_walk",
        metal::Metal { arms: SELFTESTS, judge: |b| xhci_xecp(b[0].kernel().text()) },
    ),
    (
        // Same: the crafted nine are the point, and beside them the parser
        // binds a boot stick off a descriptor a real controller delivered.
        "xhci_descriptor_walk",
        metal::Metal { arms: SELFTESTS, judge: |b| xhci_descriptors(b[0].kernel().text()) },
    ),
    (
        // No drain on this side: the whole boot's records are on the stick, so
        // the probe's line is either in them or it never ran.
        "sysret_ss_reload",
        metal::Metal { arms: SELFTESTS, judge: |b| sysret_ss(b[0].kernel().text()) },
    ),
    (
        "input_merge",
        metal::Metal { arms: SELFTESTS, judge: |b| input_merge_ok(b[0].kernel().text()) },
    ),
    (
        "operation_nesting",
        metal::Metal {
            arms: SELFTESTS,
            judge: |b| operation_nesting_log(b[0].kernel().text()),
        },
    ),
];

/// The boot most of the first tranche rides: the plain `tests/testcases` shape
/// with a job list that ends it.
///
/// Order matters. `log-close` is last because it is a *builtin*: its exit code
/// reaches no kernel record, so the runner ends the boot on a non-zero one and
/// every later job's record would then be missing for the wrong reason. The
/// last job before it is [`LOG_CLOSE_MARKER`]'s subject.
const TESTCASES: &[metal::Arm] = &[metal::once(
    "testcases",
    "tests/testcases",
    &[],
    &[
        "test_rs_wake_storm_cost",
        // Before any client connects, which is `audio_idle_suspend`'s premise.
        "test_rs_audio_idle_suspend",
        "test_rs_audio_tone",
        "test_rs_hda_client_stall",
        "test_rs_abuse_short_sleep",
        "test_rs_syscall_cost",
        "test_rs_null_sink_client_exits",
        "log-close",
    ],
)];

/// **Two boots of one config, because these two cannot share one.** Each fills
/// a machine-wide cap and leaves it filled: `mkdir_cap` fills the directory cap,
/// and `readdir_bound`'s own `create_dir("/tmp/empty")` is then refused with
/// `OutOfMemory` and it panics — measured on the first staged image, and the
/// reason each has a boot of its own in QEMU too.
const TESTCASES_MKDIR: &[metal::Arm] =
    &[metal::once("testcases-mkdir", "tests/testcases", &[], &["test_rs_mkdir_cap"])];

const TESTCASES_READDIR: &[metal::Arm] =
    &[metal::once("testcases-readdir", "tests/testcases", &[], &["test_rs_readdir_bound"])];

const JOBCASE: &[metal::Arm] = &[metal::once("jobcase", "tests/jobcase", &[], &[])];

/// A `logd` that leaves soundd's ring unread until the job says the tone played.
const LOGSTALLCASE: &[metal::Arm] =
    &[metal::once("logstallcase", "tests/logstallcase", &[], &["test_rs_soundd_log_stall"])];

/// The two boots the reset ruling is judged on, and both are boots this suite
/// already flashes: the device boot for a reset with megabytes behind it, and
/// `jobcase` for one with nothing.
const USB_RESET_BOOTS: &[metal::Arm] = &[
    metal::once(
        devices::BOOT,
        devices::CONFIG,
        &[],
        devices::JOBS,
    ),
    metal::once("jobcase", "tests/jobcase", &[], &[]),
];

const METALCASE: &[metal::Arm] = &[metal::once("metalcase", "tests/metalcase", &[], &[])];

/// The cable's own boot: netd in front of the T14's I219, and one job that
/// holds the machine up long enough for the host to reach it. The one arm in
/// this suite that names a PCI function for the loop to reach the boot over.
const LANCASE: &[metal::Arm] =
    &[metal::Arm { nic: Some(lan::NIC), ..metal::once(lan::BOOT, lan::CONFIG, &[], lan::JOBS) }];

/// The cable's boot with netd's delivery actuator armed. **A count of no
/// messages is two facts** — a part nothing made speak and a message that
/// reached no CPU — so this boot asks the part for a message and `LANCASE` does
/// not. It names no PCI function: its judge reads the kernel's own records and
/// asks the cable nothing.
const LANICSCASE: &[metal::Arm] = &[metal::once(lan::ICS_BOOT, lan::ICS_CONFIG, &[], lan::JOBS)];

/// The cable's boot with netd's lease probe armed: netd's exit code is the
/// lease's verdict, read out of the kernel's own `exit:` record, and its report
/// is on the log volume. It names the I219 for the loop to ping over the cable,
/// as [`LANCASE`] does.
const LANLEASECASE: &[metal::Arm] = &[metal::Arm {
    nic: Some(lan::NIC),
    ..metal::once(lan::LEASE_BOOT, lan::LEASE_CONFIG, &[], lan::JOBS)
}];

/// The boot the host talks to over its own cable: the loop reads the log it
/// serves under its name, pings it, runs a command on it and tells it to
/// reboot.
const LANTALKCASE: &[metal::Arm] = &[metal::Arm {
    talk: true,
    ..metal::once(lan::TALK_BOOT, lan::TALK_CONFIG, &[], lan::TALK_JOBS)
}];

/// The talking boot's config with netd swapped while it runs: the image
/// streams and authorizes a key as the talking boot's does, and the loop that
/// flashes it is not told `--talk` — the `--swap` invocation beside it owns the
/// listener. Its one job holds the machine until that invocation hands it back
/// with `reboot`, the runner's bound standing behind it.
const LANSWAPCASE: &[metal::Arm] = &[metal::Arm {
    swap: Some("netd"),
    ..metal::once("lanswapcase", lan::TALK_CONFIG, &[], common::swap::HOLD_JOBS)
}];

/// One boot for every in-kernel self-test that logs its verdict at init and
/// does nothing else.
///
/// **Eleven actuators in one image.** They cost the machine one flash between
/// them because none of them changes what the machine *is*: each stages inputs
/// the hardware cannot produce — a crafted capability list, a malformed
/// descriptor, a vector nothing claims — runs a check over them and prints a
/// count.
const SELFTESTS: &[metal::Arm] = &[metal::once(
    "selftests",
    "tests/testcases",
    &[
        "pci-cap-selftest",
        "process-reopen-selftest",
        "revoked-backing-selftest",
        "leak-rollback-selftest",
        "lapic-spurious-selftest",
        "unclaimed-vector-selftest",
        "xhci-xecp-selftest",
        "xhci-descriptor-selftest",
        "sysret-ss-probe",
        "test-input-merge",
        "sched-operation-nesting",
    ],
    &[],
)];

/// The device boot: the whole `metalprobe` job list on the T14's own devices.
/// Its own image, because every job in it either claims the display or moves
/// megabytes across the boot stick, and neither shares well.
const METALDEVICECASE: &[metal::Arm] = &[metal::once(
    devices::BOOT,
    devices::CONFIG,
    &[],
    // The same list the committed config carries, so the image the driver
    // flashes and the one the QEMU arm boots run the same jobs in the same
    // order — `devices::the_config_runs_exactly_these_jobs` is what holds the
    // two together.
    devices::JOBS,
)];

/// The boots every discovered Rust binary rides on the T14: the shipping
/// kernel's, and [`ACTUATOR_TESTS`] on the kernel that carries `SYS_DEBUG`.
fn shared_metal(keep: impl Fn(&str) -> bool) -> Vec<metal::SharedBoot> {
    let (debug, shipping): (Vec<String>, Vec<String>) = discover_rust_tests()
        .into_iter()
        .partition(|name| ACTUATOR_TESTS.contains(&name.as_str()));
    vec![
        metal::SharedBoot {
            boot: "shared".to_string(),
            config: "tests/testcases",
            params: &[],
            features: &[],
            members: const { std::num::NonZeroUsize::new(38).expect("a chunk holds a member") },
            jobs: shipping
                .iter()
                .filter(|n| keep(n))
                .map(|n| format!("test_rs_{n}"))
                .collect(),
            files: Vec::new(),
            links: Vec::new(),
        },
        // The same list's other half, on the kernel that carries `SYS_DEBUG`.
        // A second boot rather than a second image for the whole set: what
        // these need is a syscall number the rest must not have, and a boot
        // where every binary could call it would stop being the shipping
        // machine for the other seventy.
        metal::SharedBoot {
            boot: "shared-debug".to_string(),
            config: "tests/testcases",
            params: &[],
            features: toyos_build::build::TEST_KERNEL,
            members: const { std::num::NonZeroUsize::new(18).expect("a chunk holds a member") },
            jobs: debug.iter().filter(|n| keep(n)).map(|n| format!("test_rs_{n}")).collect(),
            files: Vec::new(),
            links: Vec::new(),
        },
    ]
}

/// **One boot for both measurements this suite takes rather than reads.**
/// `tests/latencycase` is the only config that endows the real-time band, and
/// the TLB bench is a boot parameter rather than a job — so arming this image
/// with it costs the machine nothing and saves a whole minute. The bench runs
/// on the BSP between the roster's release and the idle loop, before either job
/// starts, so what it spends is boot time and not latency.
const LATENCYCASE: &[metal::Arm] = &[metal::once(
    "latencycase",
    "tests/latencycase",
    &["tlb-shootdown-bench"],
    &["test_rs_cyclictest", "test_rs_sched_stress"],
)];

/// The C corpus on the T14: one boot, one job per case, each judged in the
/// guest.
///
/// **Every case in the corpus `return 0`s unconditionally**, so a bare
/// exit-code verdict would be vacuous — the comparison is the whole point. It
/// happens in `ccheck`, which runs the case with its stdout on a pipe, compares
/// the bytes with the committed `.expect` staged beside it, and exits with the
/// verdict. What crosses is that exit code, as the kernel's own record.
///
/// **One binary, one symlink per case.** `ccheck` reads `argv[0]` to know which
/// case it is, so the kernel records each run under the case's own name and a
/// host reading the stick can say which of a hundred and nineteen failed. A
/// job list of a hundred and nineteen `ccheck`s would leave one name and a
/// hundred and nineteen records told apart only by position.
///
/// It is also a *stronger* comparison than the host's. `check_c_result` reads a
/// console every process on the machine shares and has to take the other
/// writers' lines out before comparing; this reads one pipe only the case can
/// write to, so there is nothing to filter and no line that can be attributed
/// to the wrong writer.
///
/// The C cases that do **not** go on the T14, and what each one's boot said.
///
/// Measured, like `METAL_SKIP`: the whole corpus was staged onto one
/// metal-shaped image and booted, and this is what the guest comparator could
/// not answer for.
const C_METAL_SKIP: &[(&str, &str)] = &[(
    "90_stdio_buffering",
    "its expectation carries a `stderr line`, and the guest comparator reads one pipe. The \
     host compares a console both streams land on in real time; two pipes here would carry \
     the same bytes in an order nothing preserves, so this case stays where the console is",
)];

/// **One comparison rule, in two places that cannot share code.**
///
/// The host's is `tests/common/console.rs`'s `verdict` and the guest's is
/// `tests/toyos-rust-tests/src/bin/ccheck.rs`'s; a guest binary cannot link the
/// harness, so the rule is written twice and held together here by reading both
/// sources. It is `trim_end` on both sides today, and the day one of them stops
/// being that this reds and names the other.
fn the_two_comparisons_use_one_rule() -> Result<(), String> {
    let root = compile::repo_root();
    let pair = [
        ("tests/common/console.rs", "mine.trim_end() != expected.trim_end()"),
        ("tests/toyos-rust-tests/src/bin/ccheck.rs", "fn trim_end(bytes: &[u8]) -> &[u8] {"),
    ];
    for (file, rule) in pair {
        let at = root.join(file);
        let source = std::fs::read_to_string(&at).map_err(|e| format!("{}: {e}", at.display()))?;
        if !source.contains(rule) {
            return Err(format!(
                "{} no longer spells {rule:?}. The C corpus is compared on the host and again \
                 in the guest, and the two rules have to be the same one or a case passes on \
                 one machine and reds on the other",
                at.display()
            ));
        }
    }
    Ok(())
}
fn c_corpus_metal(
    c_bins: &[(String, Vec<u8>)],
    keep: impl Fn(&str) -> bool,
) -> metal::SharedBoot {
    let mut jobs = Vec::new();
    let mut files = Vec::new();
    let mut links = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for (case, data) in c_bins {
        if !keep(case) || C_METAL_SKIP.iter().any(|(name, _)| name == case) {
            continue;
        }
        // A case with no committed expectation is one nothing could judge, and
        // shipping it would be a job that passes by comparing nothing.
        let Some(expected) = c_expectation(case) else { continue };
        // **The kernel truncates a process name**, so two cases whose names
        // agree that far would land under one record. Refused rather than
        // reported, because the second one's verdict would be read as the
        // first's.
        let recorded = bootlog::recorded_name(case);
        if let Some(other) = seen.insert(recorded.clone(), case.clone()) {
            panic!(
                "the C cases {other:?} and {case:?} are both recorded as {recorded:?}, so one \
                 boot's log cannot tell their verdicts apart"
            );
        }
        files.push((format!("expect/{case}"), expected.into_bytes()));
        files.push((format!("bin/test_c_{case}"), data.clone()));
        links.push((format!("bin/{case}"), format!("/system/bin/{CCHECK}")));
        jobs.push(case.clone());
    }
    metal::SharedBoot {
        boot: "ccorpus".to_string(),
        config: "tests/testcases",
        params: &[],
        features: &[],
        members: const { std::num::NonZeroUsize::new(90).expect("a chunk holds a member") },
        jobs,
        files,
        links,
    }
}

/// The comparator's own staged name. It is a `RUST_SKIP` helper, so discovery
/// never makes a job of it and [`shared_metal`] stages it as one.
const CCHECK: &str = "test_rs_ccheck";
/// The job `log-close` runs after, and so the anchor its evidence is read from.
const LOG_CLOSE_MARKER: &str = "test_rs_null_sink_client_exits";

/// `log-close`'s verdict, as it crosses on a machine with no console.
///
/// The builtin's own `survived=` line reaches nothing, and a builtin leaves the
/// kernel no exit record of its own. What it does leave is a child's:
/// `still_armed` spawns `/system/bin/echo` **only** after the poll outlived the
/// close. So the positive half is that record arriving after the job before it —
/// `echo` is a common enough name that a whole-log scan would be answered by
/// somebody else's — and the negative half is the boot reaching its `reboot`
/// job at all, which a builtin returning non-zero prevents.
fn log_close_survived(back: &metal::Readback) -> Result<(), String> {
    let previous = format!("{}{} pid=", bootlog::EXIT, bootlog::recorded_name(LOG_CLOSE_MARKER));
    let child = format!("{}echo pid=", bootlog::EXIT);
    back.kernel().must_say_after(&previous, &child).map_err(|why| {
        format!(
            "{why}\n`log-close` reaches `still_armed` — the one thing that spawns `echo` there — \
             only if the poll outlived the close"
        )
    })?;
    bootlog::handed_back(back.after_the_reset()?.text()).map_err(|why| why.to_string())
}

/// The renderer's two text colours, as the screendump reports them.
const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];
const ALERT: [u8; 3] = [0xFF, 0x50, 0x50];
/// And the fill a halted machine leaves behind.
const FILL_FATAL: [u8; 3] = [0x60, 0x00, 0x00];
/// The fill a boot checkpoint leaves behind. It is the only thing that tells a
/// diagnostic boot's screen from a fatal report's — both carry the same log
/// lines, and one of them means the machine died.
const FILL_BOOT: [u8; 3] = [0x00, 0x00, 0x00];

/// The T14 Gen 2's panel as the console grids it: 1080/16 rows of 1920/8
/// columns. `Profile::Metal`'s display advertises that panel over EDID, so the
/// mode its firmware sets *is* this panel — the test's screen and the laptop's
/// share one geometry. Every geometry claim `screen_diag_boot` makes is made
/// against these two numbers and not against the screen it is reading.
const T14_ROWS: usize = 1080 / 16;
const T14_COLS: usize = 1920 / 8;

/// The line `SYS_DEBUG` action 3 logs immediately before halting every CPU.
/// It exists only on a `test-actuators` kernel — every other action costs the
/// caller its own process, this one costs the machine. Kept in sync with
/// `kernel/src/syscall/debug.rs` by this comment and by screen_fatal_halt
/// failing loudly if it drifts.
const FATAL_HALT_NONCE: &str = "SYS_DEBUG: fatal halt 4b1d9e2c";

/// How far a corpus case gets before it stops, and what it says when it does.
///
/// There is no `Run`, and a [`Stage::Built`] entry is now a *decline* rather
/// than an unanswered question: every case that compiles has been run, and the
/// eight that stayed off the suite each say what their own output was.
#[derive(Clone, Copy)]
enum Stage {
    /// clang refuses it, and this is what the refusal says.
    ///
    /// Quoted so that a second defect landing on the same case cannot hide
    /// under the first.
    Refused(&'static str),
    /// It compiles, and the link does not resolve — this symbol.
    NoLink(&'static str),
    /// It builds. The decision is only not to run it.
    Built,
}

/// Why a case is not run.
enum Why {
    /// Considered and declined. Nothing is owed: a *decline* is not owed to
    /// anybody by construction. A case held open by a write-up
    /// instead carries an `Open(path)` variant, revived when one needs it.
    Declined(&'static str),
}

impl Why {
    fn stated(&self) -> String {
        match self {
            Why::Declined(reason) => format!("declined: {reason}"),
        }
    }
}

/// A corpus case the suite does not run.
///
/// One list, because "is this declined or is it broken" and "how far does it
/// get" are two questions, and the two lists this replaces each answered one
/// of them for a different set of cases. `C_SKIP` was 32 names that nothing
/// ever attempted: 17 of them compiled fine, several stated a reason that was
/// not the reason — `03_struct` said `_Generic` and stopped on
/// `__attribute__((cleanup))`, `123_vla_bug` said "VLA codegen bug" and built
/// — and a name that no longer matched a file would have left a dead
/// exemption behind for ever.
///
/// **Every entry is attempted to its declared stage on every run.** Getting
/// further means the fix arrived and the entry goes; getting less far is a
/// regression. Both red the run: a host compile is deterministic, so one green
/// here is the whole population rather than one sample of an intermittent.
struct NotRun {
    /// The corpus file's stem. A name with no `.c` reds the run, so a rename
    /// takes its entry with it.
    case: &'static str,
    stage: Stage,
    why: Why,
}

const NOT_RUN: &[NotRun] = &[
    NotRun {
        case: "22_floating_point",
        stage: Stage::Built,
        why: Why::Declined("it prints `long double`s through `%Lf`, and libc reads a `long double` as a `double` (issues/build/libc-reads-a-long-double-as-a-double.md): every `%Lf` of a line whose `double`s filled the registers prints 0.000000"),
    },
    NotRun {
        case: "31_args",
        stage: Stage::Built,
        why: Why::Declined("its `.expect` is written for tcc's own runner, which passes it five arguments; every corpus binary here is run with none, so it prints `hello world 1` against an expected `hello world 6`. Nothing about the compiler is in it"),
    },
    NotRun {
        case: "34_array_assignment",
        stage: Stage::Refused("array type 'int[4]' is not assignable"),
        why: Why::Declined("it assigns one array to another, which C does not allow — an array is not a modifiable lvalue (C11 6.5.16p2) — and TinyCC accepts as an extension"),
    },
    NotRun {
        case: "40_stdio",
        stage: Stage::Built,
        why: Why::Declined("it writes `fred.txt` into the working directory and reads it back; the corpus runs from the read-only ROOT, so the write fails and the program prints `couldn't read fred.txt`. A writable working directory for the corpus is a harness change nothing else has needed"),
    },
    NotRun {
        case: "73_arm64",
        stage: Stage::Built,
        why: Why::Declined("AArch64's argument-passing corners, run here on x86-64: its `long double` lines print 0.0, for libc's reason in 22_floating_point"),
    },
    NotRun {
        case: "95_bitfields",
        stage: Stage::Built,
        why: Why::Declined("its expected layouts are TinyCC's, and TinyCC packs a `#pragma pack(1)` bitfield struct otherwise than GCC and clang do: `TEST 2 - PACKED` is 12 bytes there and 11 here, and every packed test after it differs the same way"),
    },
    NotRun {
        case: "95_bitfields_ms",
        stage: Stage::Built,
        why: Why::Declined("the same file under `ms_struct`, whose expected values are TinyCC's widths for an MS-layout bitfield: `fffffffffffffffe` there, `fffffffe` here"),
    },
    NotRun {
        case: "60_errors_and_warnings",
        stage: Stage::NoLink("main"),
        why: Why::Declined("a meta-test of compiler diagnostics: every branch is behind a -D the harness does not pass, so the file preprocesses to no `main`"),
    },
    NotRun {
        case: "96_nodata_wanted",
        stage: Stage::NoLink("main"),
        why: Why::Declined("seven configurations selected by a -D from tcc's own Makefile, four of which expect compiler diagnostics. The harness compiles one configuration and compares one stdout, so no compiler can make it pass"),
    },
    NotRun {
        case: "99_fastcall",
        stage: Stage::Refused("instruction requires: Not 64-bit mode"),
        why: Why::Declined("32-bit x86 — pushl %esp, pusha, __attribute((fastcall)) — and this target is x86-64"),
    },
    NotRun {
        case: "101_cleanup",
        stage: Stage::Built,
        why: Why::Declined("`main` returns its counter, 105, and a corpus case passes on exit 0; it also prints a `long double` through `%Lf`, for libc's reason in 22_floating_point"),
    },
    NotRun {
        case: "102_alignas",
        stage: Stage::Built,
        why: Why::Declined("`i8` takes its alignment from `__attribute__((aligned(16)))` on a type name inside `_Alignas`, which the case's own comment says clang does not apply, so it prints `1 1 1 0`"),
    },
    NotRun {
        case: "104_inline",
        stage: Stage::NoLink("inline_inline_undeclared"),
        why: Why::Declined("it expects tcc's reading of `inline`, which emits a definition a plain `inline` function never has in C99 and later (C11 6.7.4p7): the companion calls one no translation unit defines"),
    },
    NotRun {
        case: "106_versym",
        stage: Stage::Refused("call to undeclared function 'pthread_condattr_setpshared'"),
        why: Why::Declined("pthread condition variables shared across processes: `pthread.h` declares neither `pthread_condattr_setpshared` nor `PTHREAD_PROCESS_SHARED`"),
    },
    NotRun {
        case: "113_btdll",
        stage: Stage::NoLink("f_1"),
        why: Why::Declined("three shared libraries built from the same file under -DDLL=1,2,3 and loaded at run time; the harness builds one object and one binary"),
    },
    NotRun {
        case: "112_backtrace",
        stage: Stage::Built,
        why: Why::Declined("a meta-test of tcc's `-b` runtime: it expects `RUNTIME ERROR: invalid memory access` and `BCHECK: invalid pointer` lines from a bounds-checking runtime clang does not have, and prints nothing"),
    },
    NotRun {
        case: "114_bound_signal",
        stage: Stage::Refused("use of undeclared identifier 'SIGUSR1'"),
        why: Why::Declined("sigaction, sigjmp_buf and the signal numbers, which no header here declares"),
    },
    NotRun {
        case: "115_bound_setjmp",
        stage: Stage::Built,
        why: Why::Declined("`libc panic: longjmp not implemented` (`userland/libc/src/misc.rs`), exit 134. The reason the old skip list gave for this pair — setjmp — is the one claim of its kind that turned out to be right"),
    },
    NotRun {
        case: "116_bound_setjmp2",
        stage: Stage::Built,
        why: Why::Declined("the same `longjmp not implemented` panic, exit 134"),
    },
    NotRun {
        case: "126_bound_global",
        stage: Stage::Built,
        why: Why::Declined("tcc's `-b` bounds checker again: it expects `BCHECK: … is outside of the region` and `RUNTIME ERROR: invalid memory access`, and prints nothing"),
    },
    NotRun {
        case: "117_builtins",
        stage: Stage::Built,
        why: Why::Declined("its second half is behind TinyCC's `-b` bounds checker, which clang does not have, so it prints `BOUNDS OFF:` and stops"),
    },
    NotRun {
        case: "120_alias",
        stage: Stage::Refused("definition 'alias_int' cannot also be an alias"),
        why: Why::Declined("it gives a symbol a definition and an alias at once, which TinyCC allows and clang refuses"),
    },
    NotRun {
        case: "125_atomic_misc",
        stage: Stage::NoLink("main"),
        why: Why::Declined("each of its `main`s is behind a `test_*` -D the harness does not pass, so the file preprocesses to no `main`"),
    },
    NotRun {
        case: "128_run_atexit",
        stage: Stage::NoLink("on_exit"),
        why: Why::Declined("`on_exit`, a glibc extension libc does not define, and a -D per configuration to have a main at all"),
    },
];

/// Discover C tests by scanning tests/testcases/tinycc/*.c.
/// Skips companion files (contain '+') and everything in [`NOT_RUN`].
fn discover_c_tests() -> Vec<String> {
    let dir = compile::testcases_dir();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let name = e.ok()?.file_name().to_str()?.to_string();
            let stem = name.strip_suffix(".c")?;
            if stem.contains('+') {
                return None;
            }
            if NOT_RUN.iter().any(|d| d.case == stem) {
                return None;
            }
            Some(stem.to_string())
        })
        .collect();
    names.sort();
    names
}

/// Discover the Rust test binaries: every one but the [`RUST_SKIP`] helpers.
///
/// **A name that arrives this way is registered by nothing but its file.**
/// `tests/toyos-rust-tests/src/bin/<name>.rs` is the whole declaration — no row
/// here names it.
fn discover_rust_tests() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/toyos-rust-tests/src/bin");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_stem()?.to_str()?.to_string();
            (path.extension()? == "rs" && !RUST_SKIP.contains(&name.as_str())).then_some(name)
        })
        .collect();
    names.sort();
    names
}

fn compile_c_tests(names: &[String]) -> Vec<(String, Vec<u8>)> {
    // Made before the hook below goes in: a C sysroot that cannot be made is
    // no case's failure, and says so in its own words.
    compile::c_sysroot();
    // Suppress panic messages during compilation — we handle failures via catch_unwind.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    // A clang and a link per case, so the cases are spread over threads; the
    // result is in `names`' order whatever order they finish in.
    let per_thread = names.len().div_ceil(8).max(1);
    let results: Vec<(&String, Result<Vec<u8>, String>)> = std::thread::scope(|scope| {
        let workers: Vec<_> = names
            .chunks(per_thread)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|name| {
                            let built = std::panic::catch_unwind(|| {
                                compile::link_toyos(&compile::compile_c(name), name)
                            });
                            (name, built.map_err(|e| panic_message(&e)))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().expect("a compile thread")).collect()
    });

    std::panic::set_hook(prev_hook);

    let mut bins = Vec::new();
    let mut broken: Vec<(&str, String)> = Vec::new();
    for (name, built) in results {
        match built {
            Ok(linked) => bins.push((name.clone(), linked)),
            Err(why) => broken.push((name.as_str(), why)),
        }
    }

    if !broken.is_empty() {
        let mut msg = String::from(
            "a C test that is not declared in NOT_RUN stopped building, and a test that does \
             not build is a test that does not run:\n",
        );
        for (name, why) in &broken {
            msg += &format!("  c::{name}: {why}\n");
        }
        panic!("{msg}");
    }

    bins
}

/// Attempt every declared case exactly as far as it says it gets.
///
/// The list this replaces was asserted in one direction for nine names and in
/// no direction at all for thirty-two. The cost of the whole pass is a fraction
/// of a second, so nothing here is bought with test time.
fn check_not_run() {
    let dir = compile::testcases_dir();
    let mut wrong: Vec<String> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();

    compile::c_sysroot();
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));

    for entry in NOT_RUN {
        let case = entry.case;
        let before = wrong.len();
        if !seen.insert(case) {
            wrong.push(format!("{case}: named twice"));
            continue;
        }
        if !dir.join(format!("{case}.c")).is_file() {
            wrong.push(format!("{case}: no such file in the corpus — a rename left this behind"));
            continue;
        }
        let compiled = std::panic::catch_unwind(|| compile::compile_c(case));
        match (&entry.stage, compiled) {
            (Stage::Refused(says), Err(e)) => {
                let said = panic_message(&e);
                if !said.contains(says) {
                    wrong.push(format!(
                        "{case}: refused, but not for the declared reason.\n    \
                         declared: {says}\n    said:     {said}"
                    ));
                }
            }
            (Stage::Refused(says), Ok(_)) => wrong.push(format!(
                "{case}: compiles now — it was declared to stop with {says:?}. \
                 The fix arrived; delete the entry and let the case run."
            )),
            (_, Err(e)) => wrong.push(format!(
                "{case}: no longer compiles, and it was declared to get further: {}",
                panic_message(&e)
            )),
            (stage, Ok(objects)) => {
                let linked = std::panic::catch_unwind(|| compile::link_toyos(&objects, case));
                match (stage, linked) {
                    (Stage::NoLink(symbol), Err(e)) => {
                        let said = panic_message(&e);
                        if !said.contains(symbol) {
                            wrong.push(format!(
                                "{case}: the link fails on something else.\n    \
                                 declared: undefined symbol: {symbol}\n    said:     {said}"
                            ));
                        }
                    }
                    (Stage::NoLink(symbol), Ok(_)) => wrong.push(format!(
                        "{case}: links now — it was declared to fail on {symbol:?}. \
                         The fix arrived; delete the entry and let the case run."
                    )),
                    (Stage::Built, Err(e)) => wrong.push(format!(
                        "{case}: no longer links, and it was declared to build: {}",
                        panic_message(&e)
                    )),
                    (Stage::Built, Ok(_)) => {}
                    (Stage::Refused(_), _) => unreachable!("handled above"),
                }
            }
        }
        for line in &mut wrong[before..] {
            *line += &format!("\n    ({})", entry.why.stated());
        }
    }

    std::panic::set_hook(prev_hook);

    assert!(
        wrong.is_empty(),
        "NOT_RUN no longer describes the corpus. Every entry is attempted to its declared \
         stage on every run, so this is a case that moved:\n  {}",
        wrong.join("\n  "),
    );
}

/// What a caught panic said, first line, whole. A refusal quoted in `NOT_RUN`
/// is compared against this, so nothing here may shorten it.
fn panic_message(e: &Box<dyn std::any::Any + Send>) -> String {
    let full = e
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "<non-string panic>".to_string());
    full.lines().next().unwrap_or_default().to_string()
}

/// A case's committed expectation as both comparators read it, the host's and
/// the guest's `ccheck`, or `None` where none is committed. TinyCC's runner
/// captured its warnings about the case with its output, and neither compares
/// them.
fn c_expectation(case: &str) -> Option<String> {
    let at = compile::testcases_dir().join(format!("{case}.expect"));
    let text = match fs::read_to_string(&at) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!("{}: {e}", at.display()),
    };
    let warned = format!("{case}.c:");
    Some(
        text.lines()
            .filter(|l| !(l.starts_with(&warned) && l.contains(": warning: ")))
            .map(|l| format!("{l}\n"))
            .collect(),
    )
}

/// Echo what the guest actually put on screen, under `--nocapture` only —
/// it is the measurement these tests are built on.
fn print_screen(name: &str, text: &str) {
    if !qemu::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    eprintln!("        {name} decoded screen:");
    for line in text.lines() {
        eprintln!("        | {line}");
    }
}

/// Assert the colour decisions `text()` cannot see: the fill, every row an
/// `alert!` produced, and one row it did not.
///
/// **Both rows are named by their text, and that is the whole assertion.**
/// Nothing in the message says "alert" any more — the colour is the record's
/// `Level` — so a version of this that picked the ordinary row as "the first
/// one that is not red" asserted only that the palette has two colours in it,
/// and passed on a panel where every row was red. The comparison row has to be
/// chosen by something the paint cannot influence, so the caller names it.
///
/// `alert_lines` is a list because **a record is not a line**: `PanicInfo`'s
/// `Display` writes `panicked at <site>:`, a newline, and then the panic's own
/// text, so one `alert!` produces two rows and both of them are the record's.
/// A renderer that counted records where the panel counts newlines painted the
/// first red and the second white, and shifted every bit below it.
fn check_colors(
    dump: &screen::Ppm,
    fill: [u8; 3],
    alert_lines: &[&str],
    plain_line: &str,
) -> Result<(), String> {
    if dump.fill() != fill {
        return Err(format!("fill is {:?}, want {fill:?}", dump.fill()));
    }
    let rows = dump.rows();
    for alert_line in alert_lines {
        let Some(cy) = dump.row_index(alert_line) else {
            return Err(format!("{alert_line:?} not on screen\n{}", dump.text()));
        };
        if dump.row_fg(cy) != Some(ALERT) {
            return Err(format!(
                "{alert_line:?} drawn in {:?}, want alert {ALERT:?} — every row of an \
                 `alert!` record wears its level, including the ones its message wrapped \
                 or newlined onto\n{}",
                dump.row_fg(cy),
                dump.text()
            ));
        }
    }
    let Some(plain) = dump.row_index(plain_line) else {
        return Err(format!(
            "{plain_line:?} is not on screen, so there is no ordinary row to compare the \
             highlight against\n{}",
            dump.text()
        ));
    };
    if dump.row_fg(plain) != Some(WHITE) {
        return Err(format!(
            "ordinary row {:?} drawn in {:?}, want white {WHITE:?}",
            rows[plain],
            dump.row_fg(plain)
        ));
    }
    Ok(())
}

/// `tests/toyos-rust-tests`' binary that `tests/virtjobcase` runs as its job
/// `test_rs_abuse_readonly_copyout`.
const VIRT_COPYOUT: &str = "abuse_readonly_copyout";

fn virt_copyout(arch: toyos_build::arch::Arch) -> &'static [u8] {
    use toyos_build::arch::Arch;
    static X86_64: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    static AARCH64: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let built = match arch {
        Arch::X86_64 => &X86_64,
        Arch::Aarch64 => &AARCH64,
    };
    built.get_or_init(|| {
        qemu::build_toyos_bin(arch, &compile::repo_root().join("tests/toyos-rust-tests"), VIRT_COPYOUT)
    })
}

/// Boot `tests/virtjobcase` on one CPU and judge its job `job`: it ends with
/// exit 0, having said `said`. One CPU because `preempt` and `fp_isolation`
/// see a sibling run only when it took theirs. The kernel carries `SYS_DEBUG`
/// for `debug_refused`, and every job runs in every boot of the case.
fn virt_job(profile: qemu::Profile, job: &str, said: &str) -> Result<(), String> {
    let config = compile::repo_root().join("tests/virtjobcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    let qemu = QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            profile,
            smp: 1,
            kernel_features: toyos_build::build::TEST_KERNEL,
            ready_marker: "control registers: SCTLR_EL1=",
            extra_root_files: vec![(format!("bin/test_rs_{VIRT_COPYOUT}"), virt_copyout(profile.arch()).to_vec())],
            ..Default::default()
        },
    );
    judge_virt_job(qemu, job, said).map(drop)
}

/// Wait for `job`'s end on a guest booted with it, and judge it: it ends with
/// exit 0, having said `said`. Answers everything the PL011 carried.
fn judge_virt_job(mut qemu: QemuInstance, job: &str, said: &str) -> Result<String, String> {
    let end = format!("===TEST_END {job} ");
    let mut rest = String::new();
    let waited = await_marker(&mut qemu, &mut rest, &end, &format!("the job {job} to end"));
    let serial = format!("{}\n{rest}", qemu.boot_log());
    if let Err(why) = waited {
        return Err(format!("{why}\nserial:\n{serial}"));
    }
    let ended = serial
        .lines()
        .find(|l| l.contains(&end))
        .expect("await_marker answered Ok, so the marker is in what it drained");
    let Some(line) = serial.lines().find(|l| l.contains(said)) else {
        return Err(format!("{said:?} not on the PL011 ({ended})\nserial:\n{serial}"));
    };
    eprintln!("  [virt] {line}");
    if !ended.contains(&format!("===TEST_END {job} exit=0===")) {
        return Err(format!("{ended}\nserial:\n{serial}"));
    }
    Ok(serial)
}

/// What toybox's `unmap_touch` says once every read of a page just unmapped,
/// on the unmapping thread and on another, ended its process.
const UNMAP_TOUCH_SAID: &str =
    "unmap_touch: 4 reads of a page just unmapped on the unmapping thread, and 4 on another";

/// The CPUs `virt_smp` boots.
const VIRT_CPUS: u32 = 8;

/// Boot `tests/virtsmpcase` under `profile` on `cpus` CPUs, with `params` armed.
fn boot_virt_smp(profile: qemu::Profile, cpus: u32, params: &'static [&'static str]) -> QemuInstance {
    let config = compile::repo_root().join("tests/virtsmpcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            profile,
            smp: cpus,
            kernel_params: params,
            ready_marker: "control registers: SCTLR_EL1=",
            ..Default::default()
        },
    )
}

/// Boot `tests/virtsmpcase` on [`VIRT_CPUS`] CPUs under `profile`, whose
/// firmware enters every CPU at EL`el` and whose FADT names PSCI's `conduit`:
/// each CPU is started by `CPU_ON`, holds the control-register declaration as
/// entered there and joins the scheduler, and the case's job `unmap_touch`
/// ends with exit 0.
fn virt_smp(profile: qemu::Profile, conduit: &str, el: u32) -> Result<(), String> {
    let serial = judge_virt_job(boot_virt_smp(profile, VIRT_CPUS, &[]), "unmap_touch", UNMAP_TOUCH_SAID)?;
    let psci = serial.lines().find(|l| l.contains("PSCI: ")).unwrap_or_default();
    if !psci.contains(&format!(" through {conduit}")) {
        return Err(format!("PSCI is not said to be reached through {conduit}: {psci:?}\nserial:\n{serial}"));
    }
    let mut want = vec![format!("SMP: {VIRT_CPUS} of {VIRT_CPUS} MADT CPUs online")];
    for cpu in 1..VIRT_CPUS {
        want.push(format!("SMP: cpu{cpu} mpidr={cpu:#x} online"));
        want.push(format!("CPU {cpu}: joining scheduler"));
    }
    for want in want {
        if !serial.contains(&want) {
            return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
        }
    }
    let entered = format!("as declared; entered at EL{el}");
    for cpu in 0..VIRT_CPUS {
        if !serial.lines().any(|l| record_cpu(l) == Some(cpu) && l.contains(&entered)) {
            return Err(format!("cpu{cpu} never said its registers are {entered:?}\nserial:\n{serial}"));
        }
    }
    eprintln!("  [virt] {VIRT_CPUS} CPUs entered at EL{el}, started through {conduit}, and scheduling");
    Ok(())
}

/// `smp_failed_ap_leaves_no_hole` on AArch64: `smp-skip-ap` keeps `CPU_ON`
/// from the CPU that would be cpu2 of four, and the bring-up stops there, so
/// cpu2's id goes to no CPU behind it. The case's job, which the scheduler
/// places across the CPUs that came up, ends with exit 0.
fn virt_failed_ap_leaves_no_hole(profile: qemu::Profile) -> Result<(), String> {
    const CPUS: u32 = 4;
    let qemu = boot_virt_smp(profile, CPUS, &["smp-skip-ap"]);
    let serial = judge_virt_job(qemu, "unmap_touch", UNMAP_TOUCH_SAID)?;
    // The premise, not just a small machine: cpu1 came up and cpu2 did not.
    for premise in ["SMP: cpu1 mpidr=0x1 online", "SMP: cpu2 mpidr=0x2 did not echo within"] {
        if !serial.contains(premise) {
            return Err(format!("{premise:?} not on the PL011, so no non-last AP failed\nserial:\n{serial}"));
        }
    }
    let online = format!("SMP: 2 of {CPUS} MADT CPUs online");
    if !serial.contains(&online) {
        return Err(format!("{online:?} not on the PL011\nserial:\n{serial}"));
    }
    for phantom in ["CPU 2: joining scheduler", "CPU 3: joining scheduler"] {
        if serial.contains(phantom) {
            return Err(format!(
                "a CPU past the failed AP joined, so `0..cpu_count()` is not the online set: {phantom:?}\nserial:\n{serial}"
            ));
        }
    }
    eprintln!("  [virt] a non-last AP never started and the dense machine ran its job");
    Ok(())
}

/// [`panic_halts_the_others_first`] on AArch64: `tests/virtpaniccase` runs
/// `test_rs_panic_halts_first` as its one job on [`STOP_CPUS`] CPUs, and the
/// halt SGI stops every CPU but the one going fatal.
fn virt_fatal_halts_the_others_first(profile: qemu::Profile) -> Result<(), String> {
    let config = compile::repo_root().join("tests/virtpaniccase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    let fatal = qemu::build_toyos_bin(profile.arch(), &compile::repo_root().join("tests/toyos-rust-tests"), "panic_halts_first");
    let qemu = QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            profile,
            smp: STOP_CPUS,
            kernel_features: ACTUATOR_KERNEL,
            qmp: true,
            ready_marker: "control registers: SCTLR_EL1=",
            extra_root_files: vec![("bin/test_rs_panic_halts_first".to_string(), fatal)],
            ..Default::default()
        },
    );
    the_others_halt_first(qemu, profile.arch())
}

/// Boot `test_config` with the kernel selftest `armed`
/// names, and judge its one line: `<param>: PASS`.
fn virt_selftest(
    profile: qemu::Profile,
    test_config: &Path,
    armed: &'static [&'static str; 1],
) -> Result<(), String> {
    let [param] = armed;
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[],
        BootOptions {
            profile,
            kernel_params: armed,
            ready_marker: "control registers: SCTLR_EL1=",
            ..Default::default()
        },
    );
    let said = format!("{param}: ");
    let rest = qemu.drain_until(Duration::from_secs(180), |l| l.contains(&said));
    let serial = format!("{}\n{rest}", qemu.boot_log());
    let Some(verdict) = serial.lines().find(|l| l.contains(&said)) else {
        return Err(format!("{param} never reported\nserial:\n{serial}"));
    };
    eprintln!("  [virt] {verdict}");
    if !verdict.contains(&format!("{param}: PASS")) {
        return Err(format!("{verdict}\nserial:\n{serial}"));
    }
    Ok(())
}

/// Run one screen test. `Err` carries the decoded screen, because a failure
/// here is almost always "the text is not what I expected" and the decoded
/// grid is the only readable form of that.
fn run_screen_test(name: &str, profile: qemu::Profile, test_config: &Path) -> Result<(), String> {
    match name {
        "screen_diag_boot" => {
            // The diagnostic boot mode, on the machine shape it exists for.
            // What is under test is not that the console renders —
            // `screen_late_panic` has that — but that a *successful* boot
            // leaves its log on the glass. `boot_checkpoint` is the only
            // painter on this path and it returns immediately once anything
            // claims DEVICE_FRAMEBUFFER, so on the flashed image the answer
            // to "why is the keyboard dead" was up for about a tenth of a
            // second. This image contains no process that can claim it.
            //
            // Same config file `--diag-boot` builds from, and no test binaries
            // on ROOT, so the image booted here is the image flashed.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("diag");
            let options = BootOptions {
                profile,
                qmp: true,
                // No test-runner in this image, so the kernel's own last phase
                // line is the marker. It says the ring drained, not that the
                // paint happened, which is why the screen is polled below.
                ready_marker: "Boot: complete",
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;
            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
            let mut console = qemu.boot_log().to_string();
            // The window the mode exists to close: on the flashed image the
            // compositor's first output landed 48 ms after `Boot: complete`.
            // So the panel is read once the last program this image starts has
            // run and exited, when anything that would claim it already has.
            await_marker(&mut qemu, &mut console, "exit: toybox", "the last program the image starts")?;
            let dump = qemu.screendump();
            let text = dump.text();
            print_screen(name, &text);

            // A fatal report carries the same log lines. Without the fill and
            // a clean console this would go green on a kernel that panicked
            // its way to the same text.
            if dump.fill() != FILL_BOOT {
                return Err(format!(
                    "screen fill is {:?}, want the boot checkpoint's {FILL_BOOT:?}\n\
                     decoded screen:\n{text}",
                    dump.fill()
                ));
            }
            serial::Serial::named("boot console", console.as_str()).must_be_clean()?;

            for want in
                ["Boot: complete", "i8042:", common::volumes::LOG_ON_CONSOLE_AND_FILE]
            {
                if !text.contains(want) {
                    return Err(format!(
                        "{want:?} is not on screen once every program the image starts had \
                         run\ndecoded screen:\n{text}"
                    ));
                }
            }
            // `screen_log_absent`'s control. This machine's log partition
            // mounted, so nothing here may be wearing the alert marker — a
            // kernel that painted it unconditionally would satisfy that gate
            // and mean nothing.
            if let Some(row) = (0..dump.rows().len()).find(|&i| dump.row_fg(i) == Some(ALERT)) {
                return Err(format!(
                    "an alert row on a boot where everything worked: {:?}\n\
                     decoded screen:\n{text}",
                    dump.rows()[row]
                ));
            }

            // A log longer than the screen is shown as its tail, and the rule
            // is that it may never be a *silent* tail: `paint` gives an
            // overflowing text a `[page n/m]` footer and `Page::Last` numbers
            // it as the last page. So either the whole log is up, or the
            // footer says out loud that it is not. Which branch runs is a
            // property of the log's length, not of the mode — this boot fits
            // today and the footer branch is the guard for when it stops
            // fitting, which the T14's shorter panel is already close to.
            let rows = dump.rows();
            let paged = rows.iter().find(|r| r.starts_with("[page "));
            match paged {
                Some(f) => {
                    let n: Vec<&str> = f
                        .trim_start_matches("[page ")
                        .trim_end_matches(']')
                        .split('/')
                        .collect();
                    if n.len() != 2 || n[0] != n[1] {
                        return Err(format!(
                            "a boot checkpoint paints the newest page, so its footer \
                             must read [page m/m]; got {f:?}"
                        ));
                    }
                }
                None => {
                    let Some(first) = console.lines().find(|l| qemu::is_kernel_line(l)) else {
                        return Err(format!("no kernel line on the console at all:\n{console}"));
                    };
                    // A fragment rather than the line: rows are wrapped at the
                    // screen's width, and a whole line can straddle two of them.
                    let fragment: String = first.chars().skip(20).take(24).collect();
                    if !text.contains(fragment.trim()) {
                        return Err(format!(
                            "no footer, so the screen claims to hold the whole log — \
                             but its first line {first:?} is not on it\n\
                             decoded screen:\n{text}"
                        ));
                    }
                }
            }

            // And the same claim against the panel that gets flashed, which is
            // smaller than this one in both directions.
            let i8042_row = dump.row_index("i8042:").expect("checked above");
            let last_text = rows
                .iter()
                .rposition(|r| !r.is_empty() && !r.starts_with("[page "))
                .unwrap_or(0);
            let above_end = last_text.saturating_sub(i8042_row);
            if above_end >= T14_ROWS {
                return Err(format!(
                    "the first `i8042:` line is {above_end} rows above the end of the \
                     log; the T14's panel holds {T14_ROWS}, so it would not be on the \
                     flashed machine's screen at all\ndecoded screen:\n{text}"
                ));
            }
            if let Some(wide) = rows[i8042_row..=last_text]
                .iter()
                .find(|r| r.chars().count() > T14_COLS)
            {
                return Err(format!(
                    "a row inside that window is {} columns wide against the panel's \
                     {T14_COLS}; it wraps there, which pushes the `i8042:` line further \
                     up than this screen shows: {wide:?}",
                    wide.chars().count()
                ));
            }

            eprintln!("  [diag] five seconds after Boot: complete, still on screen:");
            eprintln!("  [diag]   {}", rows[i8042_row]);
            eprintln!(
                "  [diag] {above_end} rows above the end of the log; the T14 panel holds {T14_ROWS}"
            );
            eprintln!(
                "  [diag] {}",
                match paged {
                    Some(f) => format!("log longer than the screen, footer reads {f}"),
                    None => "whole log on one screen, no footer".to_string(),
                }
            );
            Ok(())
        }
        "screen_console_shell" => {
            // The third boot mode, on the machine shape that gets flashed.
            // What is under test is the whole chain a question travels on a
            // machine with no serial port: the i8042 pin, the kernel's
            // translation, `/system/bin/console`, the shell's stdin, its stdout, and
            // the panel. **A test that asserted only that a prompt rendered
            // would pass on a console that cannot read the keyboard**, which
            // is exactly the path this program exists to bring up.
            //
            // Same config file `--console-boot` builds from and no test
            // binaries on ROOT, so the image booted here is the image
            // flashed — the property `screen_diag_boot` has for its mode.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("console");
            let options = BootOptions {
                profile,
                qmp: true,
                ready_marker: "console: ready",
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;
            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
            let console = qemu.boot_log().to_string();
            serial::Serial::named("boot console", console.as_str()).must_be_clean()?;

            let font = screen::ConsoleFont::load();
            // **Both, because nothing orders them.** The seed's paint and the
            // shell's first prompt are two independent writers, so a wait that
            // stopped at the prompt could sample a panel the seed had not
            // finished putting up and report it as a console that never read
            // the log.
            let dump = qemu.screendump_while(
                Duration::from_secs(30),
                Duration::from_millis(200),
                |d| {
                    let text = d.console_text(&font);
                    text.contains(CONSOLE_PROMPT)
                        && text.contains(CONSOLE_SEED_WITNESS)
                        && text.contains(CONSOLE_PROGRAM_WITNESS)
                },
            );
            let before = dump.console_text(&font);
            if !before.contains(CONSOLE_PROMPT) {
                return Err(format!(
                    "no {CONSOLE_PROMPT:?} on the panel 30 s after `console: ready`\n\
                     decoded screen:\n{before}"
                ));
            }

            // The seed. Claiming DEVICE_FRAMEBUFFER stops `boot_checkpoint`
            // painting for the rest of the boot, so a console that merely
            // cleared the screen would have traded the diagnostic that works
            // today for one that might — and this is the line the metal track
            // keeps having to read.
            if !before.contains(CONSOLE_SEED_WITNESS) {
                // **Which of the two it is, from a number the guest published
                // rather than from the panel.** `console: ready` reports the
                // bytes of log it seeded, so a blank console and a console
                // showing some other part of the log are told apart by that
                // count — the panel cannot separate them, and a message that
                // picked one sent the next reader after the wrong subsystem.
                // The byte stream and not `boot_log`: the count is on the rest
                // of the ready marker's own line, which the line channel has
                // already consumed by the time the marker ends the boot wait.
                let said = qemu.console_stream().since(0);
                // Anchored on the whole of the console's own phrase: `logd`
                // says "this boot's kernel log is …" on the same console, and
                // a search for the shorter string finds that one first.
                let seeded = said
                    .split("cells), log ")
                    .nth(1)
                    .and_then(|rest| rest.split(' ').next())
                    .and_then(|n| n.parse::<u64>().ok());
                return Err(match seeded {
                    Some(0) => format!(
                        "no `{CONSOLE_SEED_WITNESS}` line above the prompt, and `console: \
                         ready` reported 0 bytes of log: this console started blank where \
                         the diagnostic boot starts with the log\ndecoded screen:\n{before}"
                    ),
                    Some(bytes) => format!(
                        "no `{CONSOLE_SEED_WITNESS}` line above the prompt, and the console \
                         drew {bytes} bytes of log — so the log reached the scrollback and \
                         what is on the panel is some other part of it. This is not a \
                         console that started blank\ndecoded screen:\n{before}"
                    ),
                    None => format!(
                        "no `{CONSOLE_SEED_WITNESS}` line above the prompt, and no `log N \
                         bytes` on the console to say whether the seed happened at \
                         all\nboot console:\n{said}\ndecoded screen:\n{before}"
                    ),
                });
            }
            // Non-vacuity, and not a formality: a boot checkpoint paints the
            // same lines off the same ring, so on a boot where the console
            // never ran the assertion above could be satisfied by the kernel's
            // own paint. It cannot, because that paint is in `font8x16.bin`
            // and this screen decodes under the console's — which is a claim,
            // so it is checked here and in `console_self_test` rather than
            // assumed.
            let kernel_font = dump.text();
            if kernel_font.contains("i8042:") {
                return Err(format!(
                    "the kernel's own font decodes this screen, so what is up is a boot \
                     checkpoint and not the console's paint\ndecoded screen:\n{kernel_font}"
                ));
            }

            // A program's output, on the console that owns the screen, under
            // the name of the pipe it came out of.
            if !before.contains(CONSOLE_PROGRAM_WITNESS) {
                return Err(format!(
                    "no {CONSOLE_PROGRAM_WITNESS:?} on the panel: the console does not show \
                     program output under its program's name\ndecoded screen:\n{before}"
                ));
            }

            console_type_line(&mut qemu, &font, &format!("echo {CONSOLE_NONCE}"))?;

            let dump = qemu.screendump_while(
                Duration::from_secs(30),
                Duration::from_millis(200),
                |d| d.console_rows(&font).iter().any(|r| r.trim() == CONSOLE_NONCE),
            );
            let after = dump.console_text(&font);
            print_screen(name, &after);
            // A whole trimmed row, because the shell echoes what is typed:
            // `contains` would be satisfied by `/home/toy> echo zqjxk`, which
            // says the console drew a keystroke and nothing about anything
            // having run.
            if !dump.console_rows(&font).iter().any(|r| r.trim() == CONSOLE_NONCE) {
                return Err(format!(
                    "typed `echo {CONSOLE_NONCE}` at the prompt and no row of the panel is \
                     its output; the keyboard, the shell or the console did not carry it\n\
                     decoded screen:\n{after}"
                ));
            }
            if !after.contains(&format!("{CONSOLE_PROMPT} echo {CONSOLE_NONCE}")) {
                return Err(format!(
                    "the output is on screen but the echoed command line is not, so the \
                     console is not showing what was typed\ndecoded screen:\n{after}"
                ));
            }
            let rows = dump.console_rows(&font);
            // The panel carries logd's file format — `[<wall clock> secs cpuN]`, where the serial's bracket names `kernel`.
            let log_rows =
                rows.iter().filter(|r| r.contains(" cpu") && r.contains("] ")).count();
            if log_rows == 0 {
                return Err(format!(
                    "the seed witness is on the panel but no row reads as a log record, \
                     so the format this counts by has drifted again\ndecoded screen:\n{after}"
                ));
            }
            eprintln!(
                "  [console] {log_rows} kernel log rows above a prompt, and `echo \
                 {CONSOLE_NONCE}` typed on the i8042 answered on the panel"
            );
            Ok(())
        }
        "screen_panic_muted" => {
            // The machine the whole M0/M1 line exists for: metal-sim with the
            // 16550 taken away, so `uart_present()` is false, `panic_flush`
            // returns without draining anywhere, and the rendered screen is
            // the only channel the report can possibly reach. Same kernel
            // feature and same image as `screen_late_panic`, so this costs a
            // boot and no rebuild — and it is the one place the absent-UART
            // branches run at all.
            let options = BootOptions {
                profile,
                qmp: true,
                mute: true,
                kernel_params: &["test-late-panic"],
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            metal_sim_argv_check(&argv)?;
            match argv.iter().position(|a| a == "-serial") {
                Some(i) if argv.get(i + 1).is_some_and(|v| v == "none") => {}
                _ => return Err(format!("the muted profile still has a 16550: {argv:?}")),
            }
            if argv.iter().any(|a| a.contains("stdio")) {
                return Err(format!("the muted profile still has a stdio chardev: {argv:?}"));
            }

            let mut qemu =
                QemuInstance::boot_with_options(test_config, &[], &[], options);
            // Nothing announces the panic here — there is no console for a
            // marker to arrive on — so the screen is polled until it carries
            // the report. 30s covers firmware plus the root filesystem read off USB.
            let dump = qemu.screendump_until("PANIC:", Duration::from_secs(30));
            let text = dump.text();
            print_screen(name, &text);
            // The arm line is here and nowhere else: this is the machine whose
            // panel is its only account, so it is the only one whose capture
            // `halt_all_cpus` refreshes to carry it. Newest record, so it sits
            // at the foot of the same `Page::Last` the two lines above are on.
            // The bound is derived: a panel promising a minute while the kernel
            // counts something else is the failure this line exists to catch.
            let armed = format!(
                "panic: rebooting in {} s unless a key is pressed",
                toyos_tco::PANIC_BOUND_MS / 1_000
            );
            for want in ["PANIC:", "test-late-panic: on-screen console check", &armed] {
                if !text.contains(want) {
                    return Err(format!(
                        "{want:?} not on screen of a guest with no serial port at all\ndecoded screen:\n{text}"
                    ));
                }
            }
            check_colors(
                &dump,
                FILL_FATAL,
                &["PANIC:", "test-late-panic: on-screen console check"],
                "late_panic::Nest",
            )?;
            Ok(())
        }
        "virt_early_panic" => {
            // The AArch64 port's stage 3, whole: the loader on AAVMF, the entry's
            // drop and declaration, the PL011 SPCR names, the boot's survey of
            // the machine, and a panic on both channels, before the kernel
            // reaches the AArch64 userland its ROOT carries.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                &[],
                &[],
                BootOptions {
                    profile,
                    qmp: true,
                    kernel_params: &["test-early-panic"],
                    ready_marker: "EARLY PANIC:",
                    ..Default::default()
                },
            );
            let dump = qemu.screendump_until("EARLY PANIC:", Duration::from_secs(30));
            let rest = qemu.drain_until(Duration::from_secs(10), |l| l.contains(EARLY_PANIC_MESSAGE));
            let serial = format!("{}\n{rest}", qemu.boot_log());
            // What stage 3 prints before it panics: every item is a record
            // only the AArch64 side of the loader or the kernel writes.
            for want in [
                "CPU: entered at EL1",
                "serial: PL011 at",
                "control registers: SCTLR_EL1=",
                "as declared; entered at EL",
                "memory: 0x0000400",
                "ACPI: MADT GICD at 0x8000000, GIC version 3",
                "ACPI: MADT GICC uid=0 mpidr=0x0 enabled=true",
                "ACPI: GTDT timers:",
                "EARLY PANIC: panicked at",
                EARLY_PANIC_MESSAGE,
            ] {
                if !serial.contains(want) {
                    return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
                }
            }
            let text = dump.text();
            print_screen(name, &text);
            for want in ["EARLY PANIC:", "test-early-panic: on-screen console check"] {
                if !text.contains(want) {
                    return Err(format!("{want:?} not on the ramfb panel\ndecoded screen:\n{text}"));
                }
            }
            check_colors(
                &dump,
                FILL_FATAL,
                &["EARLY PANIC:", "test-early-panic: on-screen console check"],
                "ACPI: GTDT timers:",
            )?;
            Ok(())
        }
        "virt_el2_drop" => {
            // The entry's drop from EL2, which HVF never exercises: `virt` with
            // EL2 under TCG, where firmware hands the loader the CPU at EL2. A
            // loader that refuses the CPU says so and stops; a drop that leaves
            // `HCR_EL2` other than declared halts in a named refusal and says
            // nothing; one that lands anywhere but EL1 on `SP_EL1` panics in the
            // declaration's read-back. Each way the line this waits for never
            // comes.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                &[],
                &[],
                BootOptions {
                    profile,
                    kernel_params: &["test-early-panic"],
                    ready_marker: "EARLY PANIC:",
                    ..Default::default()
                },
            );
            let rest = qemu.drain_until(Duration::from_secs(10), |l| l.contains(EARLY_PANIC_MESSAGE));
            let serial = format!("{}\n{rest}", qemu.boot_log());
            for want in [
                "CPU: entered at EL2, HCR_EL2.E2H ",
                "ID_AA64MMFR4_EL1.E2H0 0x0: the kernel's entry writes E2H clear",
                "as declared; entered at EL2, HCR_EL2 read back as declared",
                "EARLY PANIC: panicked at",
                EARLY_PANIC_MESSAGE,
            ] {
                if !serial.contains(want) {
                    return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
                }
            }
            Ok(())
        }
        "virt_early_fault" => {
            // The vectors, judged by the one thing a broken table cannot do:
            // report. An undefined instruction right after the console step
            // reaches `trap::exception`, which says what was taken and panics,
            // and the panic reaches both channels. A table that is misaligned,
            // never installed, or whose entry does not reach the handler
            // leaves the guest silent, and this waits for a line that never
            // comes.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                &[],
                &[],
                BootOptions {
                    profile,
                    qmp: true,
                    kernel_params: &["test-early-fault"],
                    ready_marker: "EARLY PANIC:",
                    ..Default::default()
                },
            );
            let dump = qemu.screendump_until("EARLY PANIC:", Duration::from_secs(30));
            const FAULT_MESSAGE: &str = "synchronous from EL1 on SP_EL1: unknown reason (an undefined instruction) at 0x";
            let rest = qemu.drain_until(Duration::from_secs(10), |l| l.contains(FAULT_MESSAGE));
            let serial = format!("{}\n{rest}", qemu.boot_log());
            for want in [
                "KERNEL PANIC: synchronous from EL1 on SP_EL1: unknown reason (an undefined instruction)",
                "EARLY PANIC: panicked at",
                FAULT_MESSAGE,
            ] {
                if !serial.contains(want) {
                    return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
                }
            }
            let text = dump.text();
            print_screen(name, &text);
            if !text.contains("EARLY PANIC:") || !text.contains("undefined instruction") {
                return Err(format!("the fault's report is not on the ramfb panel\ndecoded screen:\n{text}"));
            }
            Ok(())
        }
        "virt_user_mode" => {
            // The port's stage 4, under the EL2 profile whose
            // entry also writes what the drop leaves EL2 holding: the kernel's
            // own tables, the GIC and the timer, and a process at EL0 — init,
            // whose every page arrives by a demand fault and whose spawn of
            // `logd` is a syscall the kernel answered. Emulated, and not under
            // HVF, which exposes no RNDR for the kernel's hash seed.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                &[],
                &[],
                BootOptions {
                    profile,
                    ready_marker: "control registers: SCTLR_EL1=",
                    ..Default::default()
                },
            );
            const SPAWNED: &str = "spawn: /system/bin/logd pid=";
            let rest = qemu.drain_until(Duration::from_secs(180), |l| l.contains(SPAWNED));
            let serial = format!("{}\n{rest}", qemu.boot_log());
            for want in [
                "paging: the direct map holds memory below",
                "percpu: BSP cpu_id=0",
                "GIC: v",
                "clock: the generic timer counts at",
                "spawned /system/bin/init pid=",
                SPAWNED,
            ] {
                if !serial.contains(want) {
                    return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
                }
            }
            Ok(())
        }
        "virt_timer_preempts" => {
            // Spelled in `userland/toybox/src/preempt.rs`.
            virt_job(profile, "preempt", "preempt: the counting thread was preempted twice")
        }
        "virt_fp_isolation" => virt_job(profile, "fp_isolation", "fp_isolation: v0-v31, FPCR and FPSR survived"),
        "virt_first_entry" => virt_job(profile, "first_entry", "first_entry: x1-x30 were zero"),
        "virt_unmap_touch" => virt_job(profile, "unmap_touch", UNMAP_TOUCH_SAID),
        "virt_debug_refused" => virt_job(
            profile,
            "debug_refused",
            "debug_refused: SYS_DEBUG's double fault and TLB acknowledgement delay were refused",
        ),
        "virt_readonly_copyout" => {
            virt_job(profile, &format!("test_rs_{VIRT_COPYOUT}"), "a syscall writes only where its caller could store")
        }
        "virt_irq_storm" => {
            // The CPU floods itself with SGIs until the timer has fired a
            // thousand times through the flood, then waits for every SGI it
            // sent. A tick lost or never re-armed, or an SGI lost, leaves the
            // storm running and the verdict unsaid.
            virt_selftest(profile, test_config, &["irq-storm"])
        }
        "virt_timer_floor" => virt_selftest(profile, test_config, &["timer-floor"]),
        "virt_smp" => virt_smp(profile, "SMC", 2),
        // The EL1 entry's own arm, which fetches at a physical address under
        // the bring-up root, and PSCI through `HVC`: the path HVF takes.
        "virt_el1_smp" => virt_smp(profile, "HVC", 1),
        "virt_failed_ap_leaves_no_hole" => virt_failed_ap_leaves_no_hole(profile),
        "virt_fatal_halts_the_others_first" => virt_fatal_halts_the_others_first(profile),
        "screen_fatal_behind_a_painter" => {
            // `screen_fatal_halt` with a painter holding the panel's latch and
            // never giving it back — which is what a painter is when the halt
            // IPI lands mid-paint. The actuator has Ctrl+Alt+D's report painter
            // go fatal once it holds the latch, so the fatal path meets a
            // holder beneath itself; the report must take the screen
            // regardless, and its CPU must go on to watch the reset bound,
            // which is what the paging proves.
            const HELD: &str = "panel: a painter holding the panel went fatal";
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                &[],
                &[],
                BootOptions {
                    profile,
                    qmp: true,
                    kernel_params: &["panel-painter-stalls"],
                    ..Default::default()
                },
            );
            {
                let mut input = qemu::QmpInput::open(qemu.qmp_socket());
                input.keys(&[
                    ("ctrl", true),
                    ("alt", true),
                    ("d", true),
                    ("d", false),
                    ("alt", false),
                    ("ctrl", false),
                ]);
            }
            let dump = qemu.screendump_until(HELD, Duration::from_secs(30));
            let text = dump.text();
            print_screen(name, &text);
            if !text.contains(HELD) || dump.fill() != FILL_FATAL {
                return Err(format!(
                    "a fatal path behind a painter that holds the panel left it at fill {:?} \
                     without {HELD:?}: the report never took the screen\ndecoded screen:\n{text}",
                    dump.fill()
                ));
            }
            // The pager runs only on the CPU that claimed the panel, and it is
            // the loop that watches the reset bound: a second page is its proof.
            let paged = qemu.screendump_while(Duration::from_secs(20), Duration::from_millis(200), |d| {
                d.rows().iter().any(|r| r.contains("[page ")) && d.text() != text
            });
            if paged.text() == text {
                return Err(format!(
                    "the report never paged, so no CPU is watching the reset bound\ndecoded \
                     screen:\n{text}"
                ));
            }
            Ok(())
        }
        "screen_fatal_halt_composited" => {
            // **Can a fatal panic reach the panel once a compositor owns the
            // scanout?** Three investigations into the T14 have rested on the
            // answer being yes and nothing has ever asked it. `screen_fatal_halt`
            // boots a config with no compositor, so the screen it paints is one
            // nothing else had claimed; `screen_blocked_dump` does have a
            // compositor, but Ctrl+Alt+D paints through `paint_report`, and
            // `halt_all_cpus` paints through `render` with a different fill and
            // a different source. The owner pulled his stick, waited a minute,
            // and saw the desktop unchanged — which is what this test is for:
            // if the fatal path cannot paint over a claimed framebuffer, every
            // "nothing appeared on the panel" observation to date says nothing
            // about what the kernel did.
            // Driven by `metal-panic-probe`, which is the same kernel the owner
            // flashes: a gate that staged this with SYS_DEBUG would certify a
            // path his image does not contain.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/metalcase");
            let options = BootOptions {
                profile,
                smp: 8,
                qmp: true,
                // The T14's literal shape: no console, so the panel and the
                // black box are the report's only channels. The probe is
                // time-based, so it needs no console to drive it.
                mute: true,
                kernel_params: &["metal-panic-probe"],
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;
            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);

            // The compositor has the screen *before* anything panics. Asserted
            // on the fill, exactly as `screen_blocked_dump` does: every kernel
            // paint fills with `FILL_BOOT`, so anything else is userland
            // holding the panel. Without this the test would prove that a
            // fatal panic paints a screen nobody had taken, which is what the
            // suite already knew.
            let up = qemu.screendump_while(Duration::from_secs(30), Duration::from_millis(200), |d| {
                d.fill() != FILL_BOOT
            });
            if up.fill() == FILL_BOOT {
                return Err(
                    "the compositor never took the screen, so this would have retested \
                     screen_fatal_halt on a different config"
                        .to_string(),
                );
            }

            // The probe fires 5 s after the claim; the poll is for that plus
            // the pager cycling pages.
            const MARKER: &str = "metal-panic-probe";
            let dump = qemu.screendump_while(
                Duration::from_secs(40),
                Duration::from_millis(100),
                |d| d.text().contains(MARKER),
            );
            let text = dump.text();
            print_screen(name, &text);
            if !text.contains(MARKER) {
                return Err(format!(
                    "a fatal panic never reached the panel a compositor was holding — on a \
                     machine with no serial port that is a kernel that cannot report its own \
                     death\ndecoded screen:\n{text}"
                ));
            }
            if !text.contains("PANIC:") {
                return Err(format!(
                    "the marker is on the panel without the panic banner, so this painted \
                     something other than a fatal report\ndecoded screen:\n{text}"
                ));
            }

            // **And the report is sealed where the next boot reads it, not
            // only on the panel.** A panicking kernel stops every other CPU
            // first and runs no userland again, so `/log` gets the report from
            // the next boot's loader, out of the black box: that page, read
            // here out of the halted guest's memory, is the whole of the
            // promise. Before this, the panic path kept userland running for
            // half a second to let `logd` write it, and the stick either had it
            // or the panel said it did not.
            let page = qemu.guest_memory(toyos_blackbox::PHYS, toyos_blackbox::BYTES)?;
            let page: &[u8; toyos_blackbox::BYTES] = page
                .as_slice()
                .try_into()
                .map_err(|_| "pmemsave returned the wrong length".to_string())?;
            let Some((state, _, _, sealed)) = toyos_blackbox::recover(page) else {
                return Err(format!(
                    "the black box carries nothing after a fatal panic\ndecoded screen:\n{text}"
                ));
            };
            let sealed = String::from_utf8_lossy(sealed).into_owned();
            if state != toyos_blackbox::State::Panic
                || !sealed.contains("PANIC:")
                || !sealed.contains(MARKER)
            {
                return Err(format!(
                    "the black box reads {} and {} the banner and {} the marker after a fatal \
                     panic: the report is on the panel only\n{sealed}",
                    state.named(),
                    if sealed.contains("PANIC:") { "carries" } else { "lacks" },
                    if sealed.contains(MARKER) { "carries" } else { "lacks" },
                ));
            }
            drop(qemu);
            eprintln!(
                "  [panic] the fatal report is on the panel and sealed in the black box ({} bytes)",
                sealed.len()
            );
            if dump.fill() != FILL_FATAL {
                return Err(format!(
                    "the panel still carries {:?} rather than the fatal fill, so the compositor's \
                     screen was never taken back",
                    dump.fill()
                ));
            }
            Ok(())
        }
        other => Err(format!("unknown screen test {other}")),
    }
}

/// Run a test that owns its QEMU, turning a panic into a failed test.
///
/// Every way the harness reports a dead or unreachable guest is a panic —
/// `wait_for_ready`'s boot timeout, `assert_alive`'s exit status, `Qmp`'s
/// connect and read asserts. Uncaught, one of those unwinds out of `main` and
/// the suite exits 101 with no failure list, no remaining tests and no screen:
/// the worst report for the failure class these tests exist to catch.
fn catching(f: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|e| {
        Err(e
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "the boot panicked".to_string()))
    })
}

/// Every negative claim `Profile::Metal` makes, read off the argv QEMU is
/// launched with. A claim about which devices do *not* exist is a claim about
/// this list and nothing else — no console line and no screendump can see a
/// device that is present but unused.
fn metal_sim_argv_check(argv: &[String]) -> Result<(), String> {
    if let Some(bad) = argv.iter().find(|a| a.contains("virtio")) {
        return Err(format!("metal-sim passed a virtio device to QEMU: {bad}"));
    }
    // The mechanism, not two names. `xhci::device::scan_ports` binds any
    // boot-protocol HID — keyboard, mouse or tablet — so an enumeration of the
    // two device names that happen to be in the tree today would let a
    // `usb-mouse` added for debugging break the profile's only negative claim
    // while the assertion stayed green. The boot stick is the one USB device
    // this machine has.
    let hid = argv
        .windows(2)
        .filter(|w| w[0] == "-device")
        .map(|w| w[1].as_str())
        .find(|v| v.starts_with("usb-") && !v.starts_with("usb-storage"));
    if let Some(bad) = hid {
        return Err(format!("metal-sim passed a USB device that is not the boot stick: {bad}"));
    }
    // Without this QEMU adds an e1000e with a slirp backend, an ide-cd and an
    // isa-parallel that nothing declared — and the NIC is enough to make netd
    // claim a device on the machine whose whole point is that it has none.
    // None of them appears in argv, so this flag is the only observable form
    // of their absence here; `query_pci_agreement` is the direct one.
    if !argv.iter().any(|a| a == "-nodefaults") {
        return Err("metal-sim did not pass -nodefaults; QEMU's default-device pass is back".to_string());
    }
    Ok(())
}

/// The scanout's memory type, out of the three records that decide it.
///
/// Text in, a verdict out: `PAT:`, `GOP: scanout memory type` and `shm: …
/// mapped WriteCombining into pid` are all kernel records, so the T14's
/// readback and a QEMU console are judged by this one predicate.
fn scanout_wc(console: &str) -> Result<(), String> {
    const PAT: &str = "PAT: IA32_PAT=";
    const SCANOUT: &str = "GOP: scanout memory type ";
    const MAPPED: &str = "mapped WriteCombining into pid ";

    let Some(pat) = console.lines().find(|l| l.contains(PAT)) else {
        return Err(format!("no boot programmed IA32_PAT:\n{console}"));
    };
    let Some(entry) = pat.split(" = ").nth(1) else {
        return Err(format!("{pat:?} names no type for the entry it wrote"));
    };
    if entry.trim() != "WC" {
        return Err(format!(
            "the entry the scanout's pages select reads back {entry:?}, not WC: {pat}"
        ));
    }

    let Some(scanout) = console.lines().find_map(|l| l.split(SCANOUT).nth(1)) else {
        return Err(format!("GOP never reported the scanout's memory type:\n{console}"));
    };
    // Firmware's, and deliberately not asserted: under test is that whatever
    // the range registers say combines to WC, never what OVMF chose.
    let Some(mtrr) = scanout.split("(MTRR ").nth(1).and_then(|s| s.split(',').next()) else {
        return Err(format!("{scanout:?} does not say what the MTRR held"));
    };
    let effective = scanout.split(' ').next().unwrap_or("");
    if effective != "WC" {
        return Err(format!(
            "the scanout came out {effective} over an MTRR that says {mtrr}: {scanout}"
        ));
    }

    let Some(handed) = console.lines().find(|l| l.contains(MAPPED)) else {
        return Err(format!(
            "no process was handed a write-combining mapping, so whatever the kernel gave \
             itself, the compositor is still writing through the default:\n{console}"
        ));
    };

    eprintln!("  [metal-sim] {}", pat.trim());
    eprintln!("  [metal-sim] scanout {effective} over an MTRR that says {mtrr}");
    eprintln!("  [metal-sim] {}", handed.trim());
    Ok(())
}

/// Keep collecting serial into `log` until `marker` shows up.
///
/// **A pace, not a guard.** The two remaining callers retype at the guest and
/// ask whether the answer came back in the meantime, so `false` is an ordinary
/// step of the loop rather than a finding. Anything waiting on a guest to do
/// something wants [`await_marker`], whose ceiling is the guest going quiet.
fn serial_until(
    qemu: &mut QemuInstance,
    log: &mut String,
    marker: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        log.push_str(&qemu.drain_serial(Duration::from_millis(200)));
        if log.contains(marker) {
            return true;
        }
    }
    false
}

/// How long one burst of typing has to reach the panel.
///
/// The same ceiling every console test already gives the prompt itself, and for
/// the same guest: a `/system/bin/console` that has painted a prompt and then stops
/// echoing for this long has stopped, it is not slow. Nothing expires on the
/// healthy path — the wait ends the instant the echo is there, and
/// `screendump_while_rendering` keeps waiting past the deadline while the panel
/// is still changing.
const CONSOLE_ECHO: Duration = Duration::from_secs(30);

/// The console's input line as the panel shows it: the last row that begins
/// with the prompt, trailing blanks off.
///
/// The *last*, because a command that has already run leaves its own prompt row
/// above the live one. What is *after* the input is not trimmed and must not be
/// compared against: a panel something painted behind the console's back has
/// the rest of that row in whatever colour the painter left, and a console that
/// repaints only the cells it draws never takes it back — so the echo is a
/// prefix of this row and never the whole of it.
fn console_input_row(dump: &screen::Ppm, font: &screen::ConsoleFont) -> Option<String> {
    dump.console_rows(font)
        .into_iter()
        .rev()
        .map(|row| row.trim_end().to_string())
        .find(|row| row.starts_with(CONSOLE_PROMPT))
}

/// `line` split into bursts no wider than [`QEMU_PS2_QUEUE`].
///
/// The split is on the wire cost of each character, not on its count: a shifted
/// one is four set-1 bytes and an unshifted one is two, so eight characters and
/// four characters can be the same burst.
fn ps2_bursts(line: &str) -> Vec<String> {
    let mut bursts: Vec<String> = Vec::new();
    let mut burst = String::new();
    let mut bytes = 0usize;
    for ch in line.chars() {
        let cost = qemu::scancode_bytes(ch);
        assert!(
            cost <= QEMU_PS2_QUEUE,
            "one {ch:?} is {cost} set-1 bytes against a {QEMU_PS2_QUEUE}-byte device queue, so \
             no burst can carry it whole"
        );
        if bytes + cost > QEMU_PS2_QUEUE {
            bursts.push(std::mem::take(&mut burst));
            bytes = 0;
        }
        burst.push(ch);
        bytes += cost;
    }
    if !burst.is_empty() {
        bursts.push(burst);
    }
    // The two postconditions, checked rather than argued. A burst that outruns
    // the queue is the hole this function exists to close, and a split that
    // loses a character is the same hole reached from the other side — and both
    // would show up downstream as "the guest did not do what it was told",
    // which is the misreading that put this code here.
    assert_eq!(
        bursts.concat(),
        line,
        "the burst split lost or reordered characters of {line:?}"
    );
    for burst in &bursts {
        let bytes: usize = burst.chars().map(qemu::scancode_bytes).sum();
        assert!(
            bytes <= QEMU_PS2_QUEUE,
            "the burst {burst:?} is {bytes} set-1 bytes against a {QEMU_PS2_QUEUE}-byte device \
             queue, which drops the excess one byte at a time and says nothing"
        );
    }
    bursts
}

/// Type `line` at `/system/bin/console`'s prompt and press Enter, **paced against the
/// guest's own echo and never against a wall clock**.
///
/// [`QEMU_PS2_QUEUE`] holds sixteen set-1 bytes and drops the seventeenth
/// silently, one byte at a time; nothing on either side of the wire is told. A
/// host that keeps typing while the guest is not draining therefore hands the
/// shell a command with a hole in it, and every assertion below that point is
/// about a question the guest was never asked. Both recorded
/// `screen_console_panic` failures are exactly that and nothing else: the panel
/// carried `/home/root> test_rs_TESTpanic_child 3` on 2026-08-19 (a lost shift
/// break, so four letters came back capitalised, and a lost make) and
/// `/home/root> test_rspanic_child 3` on 2026-08-23 (sixteen bytes gone in one
/// run — one queue's worth, exactly), and in both the shell answered
/// `not found` and the test blamed the panic path for a report nothing had
/// asked for.
///
/// So the line goes out in bursts no wider than that queue, and the next burst
/// waits until the panel shows the shell echoed the last one. An echoed
/// character is a byte the guest has already read out of the device, so every
/// burst starts against an empty queue and cannot overfill it: the loss is
/// closed rather than made less likely. This is the rule [`QEMU_PS2_QUEUE`]'s
/// own doc has stated since the i8042 tests learned it — every injection paced
/// against the guest's own report — applied to the one injection path that had
/// never adopted it.
///
/// The Enter is separate and unconfirmed on purpose: what it produces is the
/// caller's assertion, and a prompt that has scrolled is not an echo to match.
///
/// The echo is matched as a **prefix** of the input row, which is what lets the
/// one command in this suite that is typed onto a panel somebody painted over
/// use this: `screen_console_clear` types `clear` at a prompt whose row is green
/// from the cell after the cursor to the edge, and a whole-row comparison would
/// read that paint as a lost keystroke.
fn console_type_line(
    qemu: &mut QemuInstance,
    font: &screen::ConsoleFont,
    line: &str,
) -> Result<(), String> {
    assert!(
        !line.contains('\n'),
        "console_type_line presses Enter itself; {line:?} carries its own"
    );
    let mut typed = String::new();
    for burst in ps2_bursts(line) {
        {
            // Opened and dropped around each burst: a `-qmp …,server` socket
            // serves one monitor at a time, and the wait below is a screendump,
            // which needs the socket back.
            let mut input = qemu::QmpInput::open(qemu.qmp_socket());
            input.type_burst(&burst);
        }
        typed.push_str(&burst);
        let want = format!("{CONSOLE_PROMPT} {typed}").trim_end().to_string();
        let echoed = |dump: &screen::Ppm| {
            console_input_row(dump, font).is_some_and(|row| row.starts_with(&want))
        };
        let dump =
            qemu.screendump_while_rendering(CONSOLE_ECHO, Duration::from_millis(50), echoed);
        if !echoed(&dump) {
            return Err(format!(
                "the console never echoed what was typed at it: its input line reads {:?} and \
                 does not begin {:?}. A keystroke was lost between the host and the shell — \
                 QEMU's {QEMU_PS2_QUEUE}-byte PS/2 queue drops what a guest that is not \
                 draining cannot take, silently — so nothing below this would have been asking \
                 the guest the question it was written to ask\ndecoded screen:\n{}",
                console_input_row(&dump, font).unwrap_or_default(),
                want,
                dump.console_text(font)
            ));
        }
    }
    let mut input = qemu::QmpInput::open(qemu.qmp_socket());
    input.keys(&[("ret", true), ("ret", false)]);
    Ok(())
}

/// What a desktop that stopped answering is asked, in the order that survives
/// being asked.
///
/// Every vCPU's registers first. `HLT=1` with `IF` set in `RFL` is a machine
/// with nothing to run rather than one wedged below the interrupt layer, and
/// that is the question #156 turns on — but Ctrl+Alt+D revives a halted CPU,
/// so the dump destroys the evidence it is taken to explain. Asked in the
/// other order, both halves describe the repaired machine.
fn freeze_report(qemu: &mut QemuInstance, log: &mut String) -> String {
    let registers = {
        let mut monitor = qemu::QmpMonitor::open(qemu.qmp_socket());
        monitor.human("info registers -a")
    };
    let before = log.len();
    {
        let mut input = qemu::QmpInput::open(qemu.qmp_socket());
        input.keys(&[
            ("ctrl", true),
            ("alt", true),
            ("d", true),
            ("d", false),
            ("alt", false),
            ("ctrl", false),
        ]);
    }
    let whole = serial_until(qemu, log, "=== end of dump ===", Duration::from_secs(30));
    format!(
        "--- info registers -a, taken before this report injected anything ---\n{registers}\n\
         --- Ctrl+Alt+D{} ---\n{}",
        if whole { "" } else { ", which produced no complete report" },
        &log[before.min(log.len())..]
    )
}

/// QEMU's `PS2_QUEUE_SIZE` (`hw/input/ps2.c`) — what the device will hold. Not
/// the 256-byte `PS2_BUFFER_SIZE` array behind it, which is a migration format
/// and not a capacity.
///
/// **Past it the device drops, silently and one byte at a time.** Measured on
/// QEMU 11.1: twenty-two key transitions in a single `input-send-event` — one
/// QMP command, so the BQL is held for the whole of it and no vCPU can read
/// port 0x60 while it runs — is 26 set-1 bytes, and the guest's driver reported
/// `drain bytes=16` and nothing else, with `0 dropped, 0 overruns, 0 lost
/// edges, 0 discarded`. A key sequence is *not* queued atomically the way a
/// command reply is (`ps2_queue_2`/`_3`/`_4` refuse to split; `ps2_put_keycode`
/// does not), so the hole lands mid-sequence: the run above delivered Left's
/// `0xE0 0x4B` make and lost its `0xE0 0xCB` break. Nothing on the guest side
/// can see this, which is why every injection test here is paced against the
/// guest's own report rather than against a wall clock.
const QEMU_PS2_QUEUE: usize = 16;

/// Run the machine-shape test, which owns its QEMU: the machine shape *is* the
/// test.
fn run_machine_test(name: &str) -> Result<(), String> {
    match name {
        "usb_boot_stick_pulled" => usb::usb_boot_stick_pulled(),
        other => Err(format!("unknown machine test {other}")),
    }
}

/// Every CPU's `CR0` and `CR4`, against what a CPU running this kernel must
/// hold.
///
/// **Not the same question the kernel's own self-check asks.** That one compares
/// each CPU against the declaration, so it catches a CPU that missed it and
/// nothing else; a declaration that is wrong satisfies it on every core. The
/// bits below are spelled out here, away from the constants that produce them,
/// so the two have to agree independently — and the ones that matter are the
/// ones an AP used to arrive with: `CD`/`NW` set is caching off, `WP` clear is
/// the kernel's own read-only mappings not binding supervisor writes, `NE`
/// clear routes an unmasked x87 exception to a pin nothing listens on.
///
/// `OSXSAVE` is asserted *clear*: with it set the CPU would permit `XCR0` to
/// name components `FXSAVE64` does not save, and this kernel saves user FP
/// state with `FXSAVE64`.
///
/// Both halves, because the kernel writes both registers whole: every bit named
/// below must hold its named value, **and a bit named nowhere below may not be
/// set at all**. Silence about a bit is a hole rather than a permission.
/// The interrupt census adds up, is monotonic, and every device delivery is
/// still cpu0's.
///
/// Text in, a verdict out. Every line it reads is a kernel record, so the
/// T14's readback and a QEMU capture are judged by this one predicate — and
/// on the T14 the *stimulus* is the boot's own job list rather than two
/// commands typed at a console.
fn irq_census(capture: &str) -> Result<(), String> {
    use common::irqcensus::{Census, DEVICE_SOURCES};
    // Every line, in order, so a later census can be compared with an
    // earlier one on the same CPU.
    let mut lines: Vec<Census> = Vec::new();
    for line in capture.lines() {
        match Census::parse(line) {
            None => continue,
            Some(Ok(census)) => lines.push(census),
            Some(Err(why)) => return Err(format!("{why}\nline: {line}")),
        }
    }
    if lines.is_empty() {
        return Err(format!(
            "no `irq: cpu` census in the capture — a process exited and the kernel \
             said nothing:\n{capture}"
        ));
    }

    // 1. The law. `total` is counted by its own increment beside each
    //    source's, never derived from them, so this is a real
    //    conservation statement: a source whose increment went missing
    //    leaves the total ahead of the sum.
    for census in &lines {
        if census.total != census.sum_of_sources() {
            return Err(format!(
                "cpu{} counted {} interrupt(s) and attributed {} to sources — a source \
                 is not being counted: {census:?}",
                census.cpu,
                census.total,
                census.sum_of_sources(),
            ));
        }
    }

    // 2. Monotonic: a counter that went backwards is a torn read or a
    //    word two CPUs are writing, which is what the no-`lock` argument
    //    in `kernel/src/irq_census.rs` rests on being impossible.
    let mut newest: std::collections::BTreeMap<u32, Census> = std::collections::BTreeMap::new();
    for census in &lines {
        if let Some(prev) = newest.get(&census.cpu) {
            if census.total < prev.total {
                return Err(format!(
                    "cpu{}'s census went backwards, {} then {}: {prev:?} then {census:?}",
                    census.cpu, prev.total, census.total,
                ));
            }
        }
        newest.insert(census.cpu, census.clone());
    }

    // 3. The machine is real: the boot CPU took interrupts, and so did
    //    at least one AP — otherwise (4) says nothing.
    let cpu0 = newest
        .get(&0)
        .ok_or_else(|| format!("no cpu0 in the census: {newest:?}"))?;
    if cpu0.total == 0 {
        return Err(format!("cpu0 took no interrupts at all: {cpu0:?}"));
    }
    let aps: Vec<&Census> = newest.values().filter(|c| c.cpu != 0).collect();
    if aps.len() < 3 {
        return Err(format!(
            "a 4-CPU machine reported {} AP(s); the census cannot see them all: {newest:?}",
            aps.len()
        ));
    }
    if !aps.iter().any(|c| c.total > 0) {
        return Err(format!("no AP took a single interrupt: {newest:?}"));
    }

    // 4. **The present-state fact this whole track is about.** Every
    //    message-signalled interrupt is addressed to physical
    //    destination 0 (`drivers::pci`'s `MSG_ADDR`) and the one I/O
    //    APIC pin goes to the BSP, so no AP may have a device count at
    //    all. This is what reds the day a placement policy lands, and
    //    that red is the improvement.
    let mut delivered = 0;
    for name in DEVICE_SOURCES {
        delivered += cpu0.source(name);
        for ap in &aps {
            if ap.source(name) != 0 {
                return Err(format!(
                    "cpu{} took {} `{name}` interrupt(s); every device vector is \
                     addressed to physical destination 0, so this machine's delivery \
                     policy has changed: {ap:?}",
                    ap.cpu,
                    ap.source(name),
                ));
            }
        }
    }
    if delivered == 0 {
        return Err(format!(
            "not one device interrupt on the whole machine, so \"they are all on \
             cpu0\" is vacuous: {newest:?}"
        ));
    }

    let share = cpu0.total as f64
        / newest.values().map(|c| c.total).sum::<u64>() as f64
        * 100.0;
    eprintln!(
        "  [irq] {} cpu(s), {} interrupt(s), {delivered} of them device deliveries — \
         all on cpu0, which took {share:.1}% of everything",
        newest.len(),
        newest.values().map(|c| c.total).sum::<u64>(),
    );

    // 5. The issuer side: every `tlb` delivery a CPU's census carries
    //    must be within the issues the `tlb:` line counted — an excess
    //    is a path shooting down uncounted. The lower bound is not
    //    asserted: an issued IPI can be pending on an IF-clear target.
    let mut issued: Vec<u64> = Vec::new();
    for line in capture.lines() {
        let Some(rest) = line.split("tlb: shootdowns=").nth(1) else { continue };
        let n: u64 = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("unreadable issuer census: {line}"))?;
        issued.push(n);
        eprintln!("  [tlb] {}", line.trim());
    }
    let Some(&last_issued) = issued.last() else {
        return Err(format!(
            "no `tlb: shootdowns=` census in the capture — two process exits on a \
             4-CPU guest and the issuer side said nothing:\n{capture}"
        ));
    };
    if issued.windows(2).any(|w| w[1] < w[0]) {
        return Err(format!("the issuer census went backwards: {issued:?}"));
    }
    for census in newest.values() {
        if census.source("tlb") > last_issued {
            return Err(format!(
                "cpu{} took {} tlb IPI(s) against {last_issued} counted issue(s) — \
                 some path shoots down without being counted: {census:?}",
                census.cpu,
                census.source("tlb"),
            ));
        }
    }
    eprintln!(
        "  [tlb] {last_issued} shootdown(s) issued, deliveries per CPU {:?} — every \
         delivery accounted for",
        newest.values().map(|c| c.source("tlb")).collect::<Vec<_>>(),
    );
    Ok(())
}

/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn pci_cap_selftest(log: &str) -> Result<(), String> {
        if let Some(bad) = log.lines().find(|l| l.contains("pci cap selftest FAILED")) {
            return Err(format!("{bad}\n{log}"));
        }
        let Some(verdict) = log.lines().find(|l| l.contains("pci cap selftest")) else {
            return Err(format!("the walk's self-test never ran:\n{log}"));
        };
        // The count, not the absence of a FAILED line, which zero cases satisfy too.
        if !verdict.contains("15/15") {
            return Err(format!("not every crafted capability layout was answered: {verdict}"));
        }
        // And how those layouts *ended*, which is the split a claimed function's
        // MSI arm turns on: a kernel that reads a list ending at a link the
        // spec forbids as one that reached its terminator misses a function's
        // MSI-X table and arms MSI on the walk's guess.
        let Some(split) = log.lines().find(|l| l.contains("pci cap split")) else {
            return Err(format!("nothing said how a capability list ended:\n{log}"));
        };
        if !split.contains("13/13") {
            return Err(format!("a capability list's end was misclassified: {split}"));
        }
        // Once for the machine: it reads no real device.
        let ran = log.matches("pci cap selftest").count();
        if ran != 1 {
            return Err(format!("the self-test ran {ran} times, wanted once\n{log}"));
        }
        // And the ordinary walk beside it: QEMU's real functions were enumerated.
        if !log.contains("PCI: Enumeration complete") {
            return Err(format!("PCI enumeration did not complete on this boot\n{log}"));
        }
        eprintln!("  [pci] {}", verdict.trim());
        Ok(())
}

/// The kernel reopens init by pid after the last handle to it has gone, and
/// no kernel thread's pid opens.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn process_reopen(log: &str) -> Result<(), String> {
        for control in ["process-reopen:", "process-open-kthread:"] {
            let Some(verdict) = log.lines().find(|l| l.contains(control)) else {
                return Err(format!("{control} never ran:\n{log}"));
            };
            if !verdict.contains("PASS") {
                return Err(format!("{}\n{log}", verdict.trim()));
            }
            eprintln!("  [process] {}", verdict.trim());
        }
        Ok(())
}

/// A backing read after deletion is refused on both writable mounts, and a page-cache slot whose fill the device refused is unbound.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn read_fault_probes(log: &str) -> Result<(), String> {
        let probe = "revoke-selftest: /tmp/revoke_probe";
        let Some(verdict) = log.lines().find(|l| l.contains(probe)) else {
            return Err(format!("{probe} never ran:\n{log}"));
        };
        if !verdict.contains("PASS") {
            return Err(format!("{}\n{log}", verdict.trim()));
        }
        eprintln!("  [read-fault] {}", verdict.trim());
        Ok(())
}

/// An "acquire before a fallible step" control: the count returned to its baseline after a refused call.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn leak_rollback(log: &str) -> Result<(), String> {
        let probe = "leak-selftest: device-mint";
        let Some(verdict) = log.lines().find(|l| l.contains(probe)) else {
            return Err(format!("{probe} never ran:\n{log}"));
        };
        if !verdict.contains("PASS") {
            return Err(format!("{}\n{log}", verdict.trim()));
        }
        eprintln!("  [leak] {}", verdict.trim());
        Ok(())
}

/// The spurious vector and an unclaimed one are both gated rather than escalated to #DF.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn lapic_vectors(log: &str) -> Result<(), String> {
        for kind in ["spurious", "unclaimed"] {
            if let Some(bad) =
                log.lines().find(|l| l.contains(&format!("{kind} selftest FAILED")))
            {
                return Err(format!("{bad}\n{log}"));
            }
            let Some(verdict) =
                log.lines().find(|l| l.contains(&format!("{kind} selftest")))
            else {
                return Err(format!("the {kind} vector was never raised:\n{log}"));
            };
            // `3/3`, not the absence of a FAILED line: a self-test that never
            // ran satisfies that absence just as well.
            if !verdict.contains("3/3") {
                return Err(format!("the self-test did not reach its verdict: {verdict}"));
            }
            // The two numbers are the interrupt census's own column — the
            // handler may not log, so that column is the only report a
            // delivery has — and both are asserted: nothing raised this
            // vector before the staged one, and exactly one arrived.
            if !verdict.contains("(0 -> 1)") {
                return Err(format!(
                    "the census did not count exactly the staged delivery: {verdict}"
                ));
            }
            eprintln!("  [lapic] {}", verdict.trim());
        }
        Ok(())
}

/// Nine crafted USB configuration descriptors, parsed at init.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn xhci_descriptors(log: &str) -> Result<(), String> {
        if let Some(bad) = log.lines().find(|l| l.contains("descriptor selftest FAILED")) {
            return Err(format!("{bad}\n{log}"));
        }
        let Some(verdict) = log.lines().find(|l| l.contains("descriptor selftest")) else {
            return Err(format!("the parser's self-test never ran:\n{log}"));
        };
        // `9/9`, not "no failures": a self-test that ran zero cases would
        // satisfy the absence of a FAILED line.
        if !verdict.contains("9/9") {
            return Err(format!("not every descriptor was parsed as required: {verdict}"));
        }
        // Once for the machine. It reads no register, so a per-controller
        // run would be two verdicts about the same nine byte arrays.
        let ran = log.matches("descriptor selftest").count();
        if ran != 1 {
            return Err(format!("the self-test ran {ran} times, wanted once\n{log}"));
        }
        // And the ordinary boot beside it: the same parser bound the boot
        // stick off a descriptor a real controller delivered.
        if !log.contains("usb-storage: 1 device(s)") {
            return Err(format!("the boot stick did not bind on this boot\n{log}"));
        }
        eprintln!("  [xhci] {}", verdict.trim());
        Ok(())
}

/// Eight malformed extended-capability lists refused, and the handoff on every controller.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn xhci_xecp(log: &str) -> Result<(), String> {
        if let Some(bad) = log.lines().find(|l| l.contains("xecp selftest FAILED")) {
            return Err(format!("{bad}\n{log}"));
        }
        let Some(verdict) = log.lines().find(|l| l.contains("xecp selftest")) else {
            return Err(format!("the walk's self-test never ran:\n{log}"));
        };
        // `8/8`, not "no failures": a self-test that ran zero cases would
        // satisfy the absence of a FAILED line.
        if !verdict.contains("8/8") {
            return Err(format!("not every malformed list was refused: {verdict}"));
        }
        // And the handoff on every controller, in `take_ownership`'s words for
        // each outcome that leaves the kernel owning it: no capability
        // (QEMU's), firmware that never claimed it (the T14's), and firmware
        // that released it. Each precedes its own reset — a reset that already
        // happened is what the whole capability exists to avoid.
        const HANDED_OVER: &[&str] = &[
            "xHCI: no USB Legacy Support capability",
            "xHCI: firmware did not claim the controller",
            "xHCI: firmware released the controller",
        ];
        const KEPT: &[&str] = &[
            "xHCI: extended capability list unusable",
            "runs past the register window — no handoff",
            "xHCI: firmware still owns the controller",
        ];
        let mut handoffs = Vec::new();
        let mut pending: Option<&str> = None;
        for line in log.lines() {
            if KEPT.iter().any(|said| line.contains(said)) {
                return Err(format!("a controller was never handed over: {line}\n{log}"));
            }
            if HANDED_OVER.iter().any(|said| line.contains(said)) {
                if let Some(earlier) = pending.replace(line) {
                    return Err(format!("a handoff with no reset of its own: {earlier}\n{log}"));
                }
            } else if line.contains("xHCI: controller reset") {
                let Some(handoff) = pending.take() else {
                    return Err(format!("a controller reset before its handoff: {line}\n{log}"));
                };
                handoffs.push(handoff);
            }
        }
        if let Some(unreset) = pending {
            return Err(format!("a handoff with no reset of its own: {unreset}\n{log}"));
        }
        if handoffs.is_empty() {
            return Err(format!("no controller was handed over and reset:\n{log}"));
        }
        // A controller that still enumerates its bus afterwards.
        if !log.contains("xHCI: controller started") {
            return Err(format!("the controller did not come up:\n{log}"));
        }
        eprintln!("  [xhci] {}", verdict.trim());
        for handoff in handoffs {
            eprintln!("  [xhci] {}", handoff.trim());
        }
        Ok(())
}

use toyos_sched::watch::window::{HELD as WINDOW_HELD, STEP as WINDOW_STEP};

/// The largest count of held windows a post ended that `log` says, 0 if none.
fn window_count(log: &str) -> u64 {
    log.lines()
        .filter_map(|line| line.split_once(WINDOW_HELD))
        .filter_map(|(_, rest)| rest.split_whitespace().next()?.parse().ok())
        .max()
        .unwrap_or(0)
}

/// Whether posts landed in held windows while the canary ran, not only before,
/// off a metal boot's kernel log split at the canary's spawn record.
///
/// **Judged on the T14 and in no QEMU guest**: the actuator holds each window
/// for a budget of its own clock, so how many a post lands in is how much of the
/// host the guest had.
fn window_held_on_metal(kernel: &serial::Serial) -> Result<(), String> {
    let text = kernel.text();
    let spawned = "spawn: /system/bin/test_rs_blocking_read_stress ";
    let at = text
        .find(spawned)
        .ok_or_else(|| format!("no `{spawned}` record: the canary never ran\n{text}"))?;
    window_held(&text[..at], &text[at..])
}

/// Whether posts landed in held windows while the canary ran, not only before.
///
/// The holds during the run are at least the last count said during it, less
/// the last said before it and the `WINDOW_STEP - 1` holds after that which no
/// line says. The floor is one line's worth. One held window a post landed in
/// is already enough for `commit-ignores-notify` to deadlock the ping-pong, so
/// a run under the floor is a run whose green says nothing about the window.
fn window_held(before: &str, during: &str) -> Result<(), String> {
    let (was, now) = (window_count(before), window_count(during));
    let held = now.saturating_sub(was + WINDOW_STEP - 1);
    if held < WINDOW_STEP {
        return Err(format!(
            "watch-window held too few windows a post landed in while the canary ran: at 
             least {held} (said {was} before, {now} during), and the floor is {WINDOW_STEP} — 
             the green canary proves nothing about the window:
{during}"
        ));
    }
    eprintln!("  [watch-window] at least {held} held windows a post landed in ({was} -> {now})");
    Ok(())
}

/// `kernel/src/arch/x86_64/hw.rs`'s probe, when it could not run.
const SYSRET_SS_UNARMED: &str = "sysret-ss: probe could not arm";
/// The probe, when a switch refreshed SS from null.
const SYSRET_SS_RELOADED: &str = "sysret-ss: reloaded";
/// The probe, when SS stayed null across a switch.
const SYSRET_SS_NOT_RELOADED: &str = "sysret-ss: NOT reloaded";

/// The context switch reloads SS from null before a `sysretq` can see it.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn sysret_ss(log: &str) -> Result<(), String> {
        if log.contains(SYSRET_SS_UNARMED) {
            return Err(format!("the SS-reload probe could not arm, so it measured nothing:\n{log}"));
        }
        if log.contains(SYSRET_SS_NOT_RELOADED) {
            return Err(format!(
                "the switch did not reload SS — a sysretq here would hand userland an \
                 unusable one:\n{log}"
            ));
        }
        if !log.contains(SYSRET_SS_RELOADED) {
            return Err(format!(
                "the SS-reload probe never reported — the first syscall may not have run it:\n{log}"
            ));
        }
        eprintln!("  [sysret-ss] the switch reloads SS from null before a sysretq can see it");
        Ok(())
}

/// The input core merged what it was handed.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn input_merge_ok(log: &str) -> Result<(), String> {
        if !log.contains("input-merge: ok") {
            return Err(format!("the input core check never reported:\n{log}"));
        }
        Ok(())
}

/// An inner `scheduler::Operation` may only narrow, and its drop restores what it displaced.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn operation_nesting_log(log: &str) -> Result<(), String> {

        /// One `key=value` off a gate line, as a number.
        fn number(line: &str, key: &str) -> Result<u64, String> {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
                .ok_or_else(|| format!("no numeric {key} in {line:?}"))
        }
        /// One `key=value` off a gate line, as a flag.
        fn flag(line: &str, key: &str) -> Result<bool, String> {
            line.split_whitespace()
                .find_map(|word| word.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
                .ok_or_else(|| format!("no boolean {key} in {line:?}"))
        }

        // Both homes. A task's word is on its `TaskHandle` and a context
        // with no task uses one slot per CPU, and the two are reached by
        // different arms of `operation_slot` — so a gate that ran in one
        // place would leave the other arm unexecuted by any test at all.
        for site in ["boot", "syscall"] {
            let say = |what: &str| -> Result<String, String> {
                let needle = format!("sched-op: {site} {what}");
                log.lines()
                    .find(|line| line.contains(&needle))
                    .map(str::to_string)
                    .ok_or_else(|| format!("no {needle:?} line on this boot:\n{log}"))
            };

            let outside = say("outside")?;
            if flag(&outside, "established")? {
                return Err(format!(
                    "{site}: an operation was already established before the gate began, \
                     so nothing below is about the nesting it made: {outside}"
                ));
            }

            // Every level: what it asked for, and what the depth below it
            // recovered. The bound is the running minimum — an inner
            // establishment takes the earlier of its own deadline and its
            // parent's, and only that.
            let mut asked = Vec::new();
            let mut narrowest = u64::MAX;
            for level in 1..=3 {
                let line = say(&format!("begin level={level}"))?;
                let want = number(&line, "asked")?;
                let saw = number(&line, "observed")?;
                narrowest = narrowest.min(want);
                if saw != narrowest {
                    return Err(format!(
                        "{site}: level {level} asked for {want} ns and the depth inside it \
                         recovered {saw} ns, against the {narrowest} ns that is the \
                         earliest of it and every level above it. An establishment that \
                         observes more than its parent allowed is a caller buying itself \
                         device time by nesting: {line}"
                    ));
                }
                asked.push(want);
            }
            // The widening attempt has to have been a real one, or the line
            // above is satisfied by a scenario in which nothing was asked.
            if asked[2] <= asked[1] {
                return Err(format!(
                    "{site}: level 3 asked for {} ns inside a level 2 of {} ns, so the \
                     gate never attempted to widen and the narrowing it reports is vacuous",
                    asked[2], asked[1],
                ));
            }

            // And the restore: each drop puts back what that establishment
            // displaced rather than clearing the slot, so the operation
            // above it survives the one below ending.
            for (level, restored) in [(3, asked[1].min(asked[0])), (2, asked[0])] {
                let line = say(&format!("end level={level}"))?;
                let saw = number(&line, "observed")?;
                if saw != restored {
                    return Err(format!(
                        "{site}: with level {level} dropped the depth recovered {saw} ns \
                         and the frame above it established {restored} ns — a guard that \
                         restores something else has ended an operation its caller is \
                         still inside: {line}"
                    ));
                }
                if !flag(&line, "established")? {
                    return Err(format!(
                        "{site}: dropping level {level} left no operation established at \
                         all, and its caller is still inside one: {line}"
                    ));
                }
            }

            let last = say("end level=1")?;
            if flag(&last, "established")? {
                return Err(format!(
                    "{site}: the outermost guard dropped and an operation is still \
                     established — the slot was restored rather than cleared, so the next \
                     depth to ask would be answered a deadline nobody set: {last}"
                ));
            }
            eprintln!(
                "  [operation] {site}: {} ns narrowed to {} ns, a {} ns request changed \
                 nothing, and both drops restored",
                asked[0], asked[1], asked[2],
            );
        }
        Ok(())
}

/// The machine's kernel thread is hosted.
///
/// Text in, a verdict out: its line is a `log!` record, so the T14's
/// readback and a QEMU boot log are judged by this one predicate.
fn klogd_hosted(boot: &serial::Serial) -> Result<(), String> {
    boot.must_be_clean()?;
    let line = boot.must_say("kthread: klogd")?;
    eprintln!("  [kthread] {}", line.trim());
    Ok(())
}

/// Every I/O APIC this machine has, and whether its redirection table is a
/// chip's rather than a floating bus's.
///
/// Text in, a verdict out: the driver runs in Phase 2 and every line it
/// writes is a kernel record, so the T14's readback and a QEMU boot log are
/// judged by this one predicate.
fn ioapic_topology(log: &str) -> Result<(), String> {
    let units: Vec<&str> = log
        .lines()
        .filter_map(|l| l.split("ioapic: id=").nth(1))
        .collect();
    if units.is_empty() {
        return Err(format!("no `ioapic: id=` line in the boot log:\n{log}"));
    }
    // A window the machine does not decode answers 0xFFFFFFFF to
    // everything, which is a *valid-looking* unit: 256 entries, all
    // read back masked, `route` succeeds into nothing. The driver
    // drops such a unit, so its absence from the log is the assertion.
    if let Some(ignored) = log.lines().find(|l| l.contains("ioapic: id=") && l.contains("IGNORED")) {
        return Err(format!("an I/O APIC failed its plausibility gate: {ignored}"));
    }
    let mut covered: Vec<(u32, u32)> = Vec::new();
    for unit in &units {
        // `<id> at <addr> ver=<v> gsi <lo>..<hi> masked <n>/<total>`
        let ver = unit
            .split_once(" ver=0x")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|v| u32::from_str_radix(v, 16).ok())
            .ok_or_else(|| format!("no version in {unit:?}"))?;
        // Both halves of the entry count come from this register, so a
        // version that is not a chip's makes the count meaningless.
        if ver == 0x00 || ver == 0xFF {
            return Err(format!("I/O APIC version {ver:#04x} is a floating bus: {unit:?}"));
        }
        let (range, masked) = unit
            .split_once(" gsi ")
            .and_then(|(_, rest)| rest.split_once(" masked "))
            .ok_or_else(|| format!("unreadable I/O APIC line: {unit:?}"))?;
        let (lo, hi) = range
            .split_once("..")
            .ok_or_else(|| format!("no GSI range in {unit:?}"))?;
        let lo: u32 = lo.trim().parse().map_err(|_| format!("bad GSI base in {unit:?}"))?;
        let hi: u32 = hi.trim().parse().map_err(|_| format!("bad GSI top in {unit:?}"))?;
        let (n, total) = masked
            .trim()
            .split_once('/')
            .ok_or_else(|| format!("no mask count in {unit:?}"))?;
        let n: u32 = n.parse().map_err(|_| format!("bad mask count in {unit:?}"))?;
        let total: u32 = total
            .split_whitespace()
            .next()
            .unwrap_or("")
            .parse()
            .map_err(|_| format!("bad entry count in {unit:?}"))?;
        // `hi` is printed as `lo + total - 1`, so comparing them is a
        // tautology. What is checkable is the bound the driver refuses
        // past — a floating bus reports 256 here.
        if hi < lo || !(1..=240).contains(&total) {
            return Err(format!(
                "I/O APIC claims gsi {lo}..{hi}, {total} entries — not a redirection table: {unit:?}"
            ));
        }
        covered.push((lo, hi));
        // The whole reason this driver runs before the first sti: an
        // entry firmware left armed at a vector with no gate is a #GP
        // that kills the boot.
        if n != total {
            return Err(format!(
                "{n} of {total} redirection entries masked — {} left armed: {unit:?}",
                total - n
            ));
        }
    }
    // Independent of any number the log derived from another: the two
    // pins the i8042 needs have to fall inside some unit's range, or
    // `route` returns `NoUnit` and there is no PS/2 input at all.
    for gsi in [1u32, 12] {
        if !covered.iter().any(|&(lo, hi)| (lo..=hi).contains(&gsi)) {
            return Err(format!(
                "no I/O APIC covers GSI {gsi}; units cover {covered:?}"
            ));
        }
    }
    // IRQ 1 and IRQ 12 must be uncovered by the override table, or
    // the i8042 driver's identity assumption is wrong on this machine.
    let Some(isos) = log
        .lines()
        .find_map(|l| l.split("ioapic: iso bus:irq->gsi [").nth(1))
        .and_then(|r| r.split(']').next())
    else {
        return Err(format!("no `ioapic: iso` line in the boot log:\n{log}"));
    };
    // Every firmware this kernel boots on overrides at least IRQ 0, so an empty
    // table is the parse finding nothing rather than the machine having nothing.
    if isos.is_empty() {
        return Err(format!("the interrupt-source-override table is empty:\n{log}"));
    }
    eprintln!("  [ioapic] {} unit(s), overrides {isos}", units.len());
    Ok(())
}

fn control_regs(log: &str, cpus: u32) -> Result<(), String> {
    /// `(bit, name, must_be_set)`. Every bit `CR0` defines, so a value with any
    /// other bit set is reserved state the kernel put there.
    const CR0_BITS: &[(u32, &str, bool)] = &[
        (0, "PE", true),
        (1, "MP", true),
        (2, "EM", false),
        (3, "TS", false),
        (4, "ET", true),
        (5, "NE", true),
        (16, "WP", true),
        (18, "AM", false),
        (29, "NW", false),
        (30, "CD", false),
        (31, "PG", true),
    ];
    const CR4_BITS: &[(u32, &str, bool)] = &[
        (3, "DE", true),
        (5, "PAE", true),
        (6, "MCE", true),
        (9, "OSFXSR", true),
        (10, "OSXMMEXCPT", true),
        (12, "LA57", false),
        // Clear: `CR4.FSGSBASE` gates RD/WR FS/GS BASE at every CPL (Intel SDM
        // Vol. 3A §2.5, Vol. 2 `WRGSBASE`), so no Ring 3 thread aims `GS.base`.
        (16, "FSGSBASE", false),
        (18, "OSXSAVE", false),
        // Not a bit the machine may withhold: `Arch::cpu`'s two x86-64 CPUs
        // are the only x86-64 CPUs this repository launches and both name
        // `+smep`, so a boot without supervisor-mode execution prevention is a
        // kernel that stopped enabling it or a launcher that stopped asking.
        (20, "SMEP", true),
    ];
    /// The `CR4` bits the CPU may withhold, so neither answer is wrong.
    const CR4_MAY: &[(u32, &str)] = &[(11, "UMIP"), (17, "PCIDE"), (21, "SMAP")];

    let mut seen: Vec<(u32, u64, u64)> = Vec::new();
    for line in log.lines() {
        let Some(rest) = line.split("control_regs: cpu").nth(1) else { continue };
        let Some((id, rest)) = rest.split_once(" cr0=0x") else { continue };
        let Some((cr0, cr4)) = rest.split_once(" cr4=0x") else { continue };
        let (Ok(id), Ok(cr0), Ok(cr4)) = (
            id.parse::<u32>(),
            u64::from_str_radix(cr0, 16),
            u64::from_str_radix(cr4.split_whitespace().next().unwrap_or(""), 16),
        ) else {
            return Err(format!("unreadable control-register line: {line:?}"));
        };
        seen.push((id, cr0, cr4));
    }

    // Which CPUs answered, not how many lines were printed: a boot where one AP
    // never came up at all must not pass by having the BSP print twice.
    let ids: BTreeSet<u32> = seen.iter().map(|&(id, _, _)| id).collect();
    let want: BTreeSet<u32> = (0..cpus).collect();
    if ids != want {
        return Err(format!(
            "expected one line from each of {want:?}, got {ids:?}:\n{log}"
        ));
    }

    // A bit named nowhere above is as much of the declaration as a named one,
    // and the kernel writes both registers whole — so `UMIP`, `PGE`, `TSD` or
    // `PKE` would reach every CPU with nothing here to say so. Which is why
    // what follows is the set this gate has an opinion about, rather than a
    // second list of bits to forbid: a forbid-list fails open on the next bit.
    let named = |bits: &[(u32, &str, bool)], may: &[(u32, &str)]| -> u64 {
        bits.iter().fold(0, |m, b| m | 1u64 << b.0)
            | may.iter().fold(0, |m, b| m | 1u64 << b.0)
    };

    // Every wrong bit on a CPU rather than the first: an AP holding INIT's CR0
    // is wrong in five at once and each is a different consequence, so a
    // message naming one sends the next reader after a fifth of it.
    for &(id, cr0, cr4) in &seen {
        let mut wrong = String::new();
        for (reg, value, bits, known) in [
            ("cr0", cr0, CR0_BITS, named(CR0_BITS, &[])),
            ("cr4", cr4, CR4_BITS, named(CR4_BITS, CR4_MAY)),
        ] {
            for &(bit, name, set) in bits {
                if (value & (1 << bit) != 0) != set {
                    wrong += &format!(" {reg}.{name} must be {}", if set { "set" } else { "clear" });
                }
            }
            let extra = value & !known;
            if extra != 0 {
                wrong += &format!(" {reg} holds {extra:#x}, which this gate never named");
            }
        }
        if !wrong.is_empty() {
            return Err(format!("cpu{id} cr0={cr0:#010x} cr4={cr4:#010x}:{wrong}"));
        }
    }

    // A CPU that agrees about every bit named above can still differ in one
    // that is not, and a thread migrating onto it would execute differently
    // from one moment to the next.
    let (_, cr0, cr4) = seen[0];
    if let Some(&(id, other0, other4)) = seen.iter().find(|&&(_, a, b)| (a, b) != (cr0, cr4)) {
        return Err(format!(
            "cpu0 has cr0={cr0:#010x} cr4={cr4:#010x} and cpu{id} has \
             cr0={other0:#010x} cr4={other4:#010x}"
        ));
    }

    eprintln!("  [control_regs] {cpus} CPUs, cr0={cr0:#010x} cr4={cr4:#010x}");
    Ok(())
}

/// The word between `head` and `tail` on the first line of `log` carrying
/// **both**, which is how every judge below reads a field out of a record.
///
/// Both, not just the head: a boot log has many lines that begin a field name
/// and do not carry the field, and a reader that took the first of those would
/// answer about the wrong record rather than say it found none.
fn field_between<'a>(log: &'a str, head: &str, tail: &str) -> Result<&'a str, String> {
    log.lines()
        .find_map(|line| {
            let (_, rest) = line.split_once(head)?;
            let (found, _) = rest.split_once(tail)?;
            Some(found)
        })
        .ok_or_else(|| format!("no record carrying {head:?} and then {tail:?}"))
}

/// The same, parsed.
fn number_between(log: &str, head: &str, tail: &str) -> Result<u64, String> {
    let word = field_between(log, head, tail)?;
    word.trim().parse().map_err(|_| format!("{head:?} is followed by {word:?}, not a number"))
}

/// Every CPU the firmware named is scheduling, every one of them holds the
/// control-register declaration, and none of their timestamp counters trails
/// the BSP's.
///
/// **The third is the one nothing in this tree ever asked.**
/// `clock::nanos_since_boot` subtracts a single BSP-sampled origin whatever CPU
/// reads it, so a CPU whose TSC starts behind that origin saturates to zero and
/// stamps every record it writes as the oldest thing the machine has
///. The kernel
/// brackets each AP's first `rdtsc` between two of the BSP's, taken either side
/// of a bring-up that is serialised — so "inside" is what a synchronised
/// counter gives and nothing else does. **QEMU cannot refute it**: every guest
/// TSC is synthesised from one host clock, which is exactly why the assertion
/// is here to be run on metal.
fn smp_roster_and_tsc_trail(log: &str, cpus: u32) -> Result<(), String> {
    let online = number_between(log, "SMP: ", " of ")?;
    let named = number_between(log, " of ", " MADT cpus online")?;
    if online != u64::from(cpus) || named != u64::from(cpus) {
        return Err(format!(
            "the machine was given {cpus} CPUs, its MADT named {named} and {online} came up"
        ));
    }
    let bracketed = number_between(log, "MADT cpus online, ", " of ")?;
    let aps = u64::from(cpus) - 1;
    if bracketed != aps {
        let outside: Vec<&str> = log
            .lines()
            .filter(|l| l.contains(" the BSP's ") && !l.contains("inside"))
            .collect();
        return Err(format!(
            "{bracketed} of {aps} APs read a TSC inside the BSP's bracket; the rest: {outside:#?}"
        ));
    }
    let checked = number_between(log, "control_regs: ", " of ")?;
    if checked != u64::from(cpus) {
        return Err(format!("{checked} of {cpus} CPUs were checked against the declaration"));
    }
    let widest = log
        .lines()
        .filter_map(|l| l.split(" (").nth(1)?.split(" cycles wide)").next()?.parse::<u64>().ok())
        .max()
        .unwrap_or_default();
    eprintln!(
        "  [smp] {online} CPUs online, {bracketed} AP TSCs inside a BSP bracket at most \
         {widest} cycles wide"
    );
    Ok(())
}

/// The physical memory manager's accounting against the firmware map, to the
/// byte.
///
/// Every byte UEFI called usable becomes exactly one of three things: a whole
/// 2 MiB frame the bitmap manages, a frame withheld because a reserved region
/// touches it, or a fragment lost to an entry that does not begin and end on a
/// 2 MiB boundary. The kernel prints all four numbers; this re-adds them, so
/// the sum is checked against the firmware's total and not against itself.
fn pmm_accounting(log: &str) -> Result<(), String> {
    let firmware = number_between(log, "the firmware map calls ", " bytes usable")?;
    let managed = number_between(log, " managed=", " ")?;
    let withheld = number_between(log, " withheld=", " ")?;
    let lost = number_between(log, " unaligned=", ",")?;
    if managed + withheld + lost != firmware {
        return Err(format!(
            "the firmware map calls {firmware} bytes usable and the PMM accounts for \
             {managed} + {withheld} + {lost} = {}",
            managed + withheld + lost
        ));
    }
    if managed == 0 {
        return Err("the PMM manages no memory at all".to_string());
    }
    eprintln!(
        "  [pmm] {} MiB managed, {withheld} B withheld, {lost} B lost to alignment, of \
         {} MiB the firmware called usable",
        managed / (1024 * 1024),
        firmware / (1024 * 1024),
    );
    Ok(())
}

/// Every ACPI table this kernel goes on to decode was reached through a
/// checksummed RSDP and XSDT, checksummed itself, and **was decoded**.
///
/// The DMAR is not required: a guest with no `intel-iommu` publishes none, and
/// this host boots one. What is required is that a table the kernel *does* read
/// is never one it skipped the validation of, and never one it validated and
/// then did nothing with — a row saying only that four bytes and a length
/// checksummed is presence and not a decode, so each one is held to a record
/// carrying a field read out of it.
fn acpi_table_inventory(log: &str) -> Result<(), String> {
    /// Each table's signature and the record that carries a value decoded from
    /// it. A signature with no such record is a table this kernel validated and
    /// never read.
    const NEEDED: &[(&str, &str)] = &[
        ("APIC", "ACPI: MADT cpus="),
        ("FACP", "ACPI: reset register "),
        ("HPET", "clock: HPET at "),
        ("MCFG", "ACPI: ECAM base address: "),
    ];
    for (signature, decoded) in NEEDED {
        if !log.lines().any(|l| l.contains(&format!("ACPI: {signature} at ")) && l.contains("checksummed"))
        {
            return Err(format!(
                "no checksummed {signature} in the boot log; the inventory said: {:#?}",
                log.lines().filter(|l| l.starts_with("[") && l.contains("ACPI: ")).collect::<Vec<_>>()
            ));
        }
        // Never the inventory row itself, which is the presence this exists to
        // be more than.
        let decoded_line = log
            .lines()
            .find(|l| l.contains(decoded) && !l.contains("checksummed"))
            .ok_or_else(|| {
                format!(
                    "the {signature} checksummed and no record carries {decoded:?}, so nothing \
                     this boot did rests on anything decoded out of it"
                )
            })?;
        eprintln!("  [acpi] {signature} decoded: {}", decoded_line.trim());
    }
    let validated = number_between(log, "ACPI: ", " of ")
        .map_err(|why| format!("no inventory summary: {why}"))?;
    if validated < NEEDED.len() as u64 {
        return Err(format!("{validated} tables checksummed and this kernel decodes {NEEDED:?}"));
    }
    let rows: Vec<&str> = log
        .lines()
        .filter_map(|l| l.split("ACPI: ").nth(1))
        .filter(|l| l.contains("checksummed"))
        .collect();
    eprintln!("  [acpi] {validated} tables checksummed: {rows:#?}");
    Ok(())
}

/// The LAPIC timer and the TSC each calibrated to a frequency.
fn timer_calibration(log: &str) -> Result<(), String> {
    let lapic_hz = number_between(log, "ticks/10ms, so ", "Hz")?;
    if lapic_hz == 0 {
        return Err("the LAPIC timer calibrated to no frequency at all".to_string());
    }
    let measured = number_between(log, "clock: TSC measured ", "Hz against the HPET")?;
    if measured == 0 {
        return Err("the TSC calibrated to no frequency at all".to_string());
    }
    eprintln!("  [timer] TSC {measured}Hz measured; LAPIC {lapic_hz}Hz");
    Ok(())
}

/// The TSC the whole machine is timed by, against the frequency the part itself
/// states.
///
/// **The one cross-source check a boot has.** Everything else the kernel times
/// is derived from the HPET calibration, so it can only agree with itself;
/// CPUID leaf 15H's crystal ratio and leaf 16H's base frequency are the CPU's
/// own statement, arrived at by neither the HPET nor the counting loop. A part
/// that states neither is a fact about the part, not a failure, so the ppm
/// bound is asserted only where a statement exists. Judged on metal only: the
/// calibration is a span of a clock, and a guest's clock runs while its host
/// has the vCPU.
fn tsc_agrees_with_cpuid(log: &str) -> Result<(), String> {
    /// One percent, which is the widest two timebases can differ and still be
    /// counting the same second. A refusal and not a measurement: it catches a
    /// machine whose HPET and CPUID have stopped agreeing at all, and nothing
    /// narrower is true of every part this kernel may boot on.
    const CEILING_PPM: u64 = 10_000;

    let measured = number_between(log, "clock: TSC measured ", "Hz against the HPET")?;
    let Ok(stated) = number_between(log, "CPUID states ", "Hz,") else {
        let why = log
            .lines()
            .find(|l| l.contains("CPUID leaves 15H and 16H"))
            .ok_or("neither a stated frequency nor the record saying there is none")?;
        eprintln!("  [timer] TSC {measured}Hz — {}", why.trim());
        return Ok(());
    };
    let ppm = number_between(log, "Hz, ", "ppm apart")?;
    if ppm > CEILING_PPM {
        return Err(format!(
            "the TSC measures {measured}Hz against the HPET and CPUID states {stated}Hz — \
             {ppm}ppm apart, over the {CEILING_PPM}ppm this bound allows"
        ));
    }
    eprintln!("  [timer] TSC {measured}Hz measured, {stated}Hz stated, {ppm}ppm apart");
    Ok(())
}

/// Every PCI function the kernel enumerated is in the log with its identity and
/// the memory windows firmware assigned it, and the count it announced is the
/// number of rows it wrote.
///
/// The rows are what a metal profile pins a machine's inventory against; what
/// is checkable without one is that the two halves agree, which is what fails
/// when enumeration stops early or a row goes unwritten.
fn pci_inventory(log: &str) -> Result<(), String> {
    let announced = number_between(log, "PCI: Enumeration complete, ", " functions")?;
    let rows: Vec<&str> = log.lines().filter_map(|l| l.split("  PCI ").nth(1)).collect();
    if rows.len() as u64 != announced {
        return Err(format!(
            "the kernel announced {announced} functions and wrote {} rows",
            rows.len()
        ));
    }
    if announced == 0 {
        return Err("the kernel enumerated no PCI function at all".to_string());
    }
    let mut with_windows = 0;
    for row in &rows {
        // Identity and window list on one line, so an inventory is one row per
        // function rather than a join the reader has to make.
        if !row.contains("vendor=") || !row.contains("device=") || !row.contains(" bars=[") {
            return Err(format!("a PCI row is not an inventory row: {row:?}"));
        }
        if !row.contains(" bars=[]") {
            with_windows += 1;
        }
    }
    eprintln!("  [pci] {announced} functions, {with_windows} of them with assigned windows");
    Ok(())
}

/// What one machine-wide TLB shootdown costs its initiator, as a distribution
/// over a fixed count against every CPU the machine brought up.
///
/// The `tlb:` census is a sum and a maximum over whatever the boot happened to
/// unmap, so its average moves with the workload and its tail is one sample.
/// This is the same path measured under a stated stimulus, which is what makes
/// two boots comparable. **The number itself is a metal number**: this host's
/// guests are TCG, which prices an IPI and an uncontended atomic unlike
/// hardware, so what is asserted here is the shape — sorted, non-zero, and the
/// CPUs it was measured across.
fn tlb_shootdown_cost(log: &str, cpus: u32) -> Result<(u64, u64), String> {
    // Scoped to the bench's own line: the `tlb:` census carries a `max=` too,
    // in microseconds, and a reader over the whole log would take whichever
    // came first.
    let line = log
        .lines()
        .find(|l| l.contains("tlb: bench "))
        .ok_or("no `tlb: bench` record — the actuator armed nothing")?;
    // The round count is the kernel's constant and is read off the line rather
    // than restated: a bench that shrank would otherwise pass unremarked.
    let rounds = number_between(line, "tlb: bench ", " shootdowns")?;
    let across = number_between(line, " shootdowns across ", " cpus")?;
    if across != u64::from(cpus) {
        return Err(format!("the bench ran across {across} CPUs and the machine was given {cpus}"));
    }
    let read = |head: &str| number_between(line, head, "ns");
    let (min, p50, p90, p99, max) =
        (read("min=")?, read("p50=")?, read("p90=")?, read("p99=")?, read("max=")?);
    if min == 0 {
        return Err("the fastest of 256 machine-wide shootdowns took no time at all".to_string());
    }
    if !(min <= p50 && p50 <= p90 && p90 <= p99 && p99 <= max) {
        return Err(format!(
            "the distribution is not sorted: min={min} p50={p50} p90={p90} p99={p99} max={max}"
        ));
    }
    eprintln!(
        "  [tlb] {rounds} shootdowns across {across} CPUs: min={min}ns p50={p50}ns p90={p90}ns \
         p99={p99}ns max={max}ns"
    );
    Ok((p50, p99))
}

/// A negative code is `cyclictest`'s refusal and not a fast machine — the sign
/// is the whole of what separates the two, and that contract is in the binary's
/// own module header.
fn wake_latency_recorded(boot: &metal::Readback) -> Result<(), String> {
    let code = boot.exit_code("test_rs_cyclictest")?;
    if code < 0 {
        return Err(format!(
            "cyclictest exited {code}, which is a refusal and not a measurement; its own last \
             line in the boot's log says which"
        ));
    }
    boot.measured("latency.p99_us", u64::try_from(code).expect("a non-negative code"))
}

fn run_debug_mode(c_tests: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) {
    let cmd_path = Path::new("/tmp/toyos-debug-cmd");
    let result_path = Path::new("/tmp/toyos-debug-result");
    let ready_path = Path::new("/tmp/toyos-debug-ready");

    let _ = fs::remove_file(cmd_path);
    let _ = fs::remove_file(result_path);
    let _ = fs::remove_file(ready_path);

    let test_config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testcases");
    let mut qemu = QemuInstance::boot_with_options(
        &test_config,
        c_tests,
        rust_bins,
        BootOptions {
            gdb_stub: true,
            debug_wait: true,
            ..Default::default()
        },
    );

    let repo = compile::repo_root();
    let kernel_elf = repo.join(format!(
        "kernel/target/{}/{}/kernel",
        common::qemu::SUITE_ARCH.kernel(),
        toyos_build::build::PROFILE
    ));

    eprintln!();
    eprintln!("╔══════════════════════════════════════════════════════════════╗");
    eprintln!("║  QEMU running with GDB stub on localhost:1234               ║");
    eprintln!("╠══════════════════════════════════════════════════════════════╣");
    eprintln!("║  Kernel ELF: {}", kernel_elf.display());
    eprintln!("║                                                              ║");
    eprintln!("║  Send commands:                                              ║");
    eprintln!("║    echo 'run test_c_49_bracket_evaluation' > {}    ║", cmd_path.display());
    eprintln!("║    echo 'run test_rs_std_alloc' > {}               ║", cmd_path.display());
    eprintln!("║    cat {}                                 ║", result_path.display());
    eprintln!("║    echo 'quit' > {}                                ║", cmd_path.display());
    eprintln!("╚══════════════════════════════════════════════════════════════╝");

    fs::write(ready_path, "ready\n").unwrap();

    loop {
        thread::sleep(Duration::from_millis(200));

        let cmd = match fs::read_to_string(cmd_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = fs::remove_file(cmd_path);
        let cmd = cmd.trim();
        if cmd.is_empty() {
            continue;
        }

        if cmd == "quit" || cmd == "q" {
            eprintln!("[debug] Quit requested");
            let _ = fs::write(result_path, "quit\n");
            break;
        }

        if let Some(test_name) = cmd.strip_prefix("run ") {
            let test_name = test_name.trim();
            eprintln!("[debug] Running {test_name}...");
            let result = qemu.run_test(test_name, Duration::from_secs(60));

            let mut output = String::new();
            output.push_str(&format!("test: {}\n", result.name));
            output.push_str(&format!("exit_code: {:?}\n", result.exit_code));
            if let Some(err) = &result.error {
                output.push_str(&format!("error: {err}\n"));
            }
            if !result.stdout.is_empty() {
                output.push_str("--- stdout ---\n");
                output.push_str(&result.stdout);
            }
            eprintln!("[debug] {output}");
            fs::write(result_path, &output).unwrap();
        } else {
            eprintln!("[debug] Sending raw serial: {cmd}");
            writeln!(qemu.stdin_mut(), "{cmd}").expect("Failed to write to QEMU stdin");
            qemu.flush_stdin();
            fs::write(result_path, "sent\n").unwrap();
        }
    }

    let _ = fs::remove_file(ready_path);
    eprintln!("[debug] Shutting down QEMU...");
}

/// Built test binaries, each by the name `run` takes.
type Binaries = Vec<(String, Vec<u8>)>;

/// The binaries the shared boots carry, C and Rust: every Rust test binary,
/// and the C corpus, compiled once its declared cases have been attempted to
/// their stages.
fn build_shared_bins() -> (Binaries, Binaries) {
    let rust_tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/toyos-rust-tests");
    eprintln!("[toyos] Building Rust tests...");
    let rust_bins = qemu::build_toyos_bins(&rust_tests_dir);
    let c_names = discover_c_tests();
    eprintln!(
        "[toyos] Compiling {} C tests, and attempting {} declared ones...",
        c_names.len(),
        NOT_RUN.len()
    );
    check_not_run();
    (compile_c_tests(&c_names), rust_bins)
}

/// What one worker takes off the queue: one test, and every boot it owns.
#[derive(Clone)]
enum Task {
    Machine(&'static str),
    Screen(&'static str, qemu::Profile),
}

/// What the suite has to say about one test once it has finished.
struct Outcome {
    name: String,
    /// `None` is a pass — but only [`Outcome::verdict`] may read it as one.
    reason: Option<String>,
    elapsed: Duration,
    /// How long the host was suspended while this test ran. A verdict taken
    /// across that is not a verdict, whichever way it came out.
    suspended: Duration,
}

/// What the suite may conclude from one outcome.
#[derive(PartialEq, Debug)]
enum Verdict {
    Pass,
    Fail,
    /// The host stopped in the middle of it. Neither a pass nor a fail: the
    /// guest, QEMU's virtual clock and every wall-clock margin the test's
    /// assertion rests on all jumped by however long the lid was closed, so the
    /// run measured something and it was not this tree.
    Invalid,
}

impl Outcome {
    fn verdict(&self) -> Verdict {
        if self.suspended >= common::clock::SUSPENDED_AT_LEAST {
            return Verdict::Invalid;
        }
        match self.reason {
            None => Verdict::Pass,
            Some(_) => Verdict::Fail,
        }
    }

    /// Whether this red is a blown liveness guard or the backstop rather than an
    /// answer.
    fn stalled(&self) -> bool {
        self.reason.as_deref().is_some_and(|r| r.contains(STALLED) || r.contains(TIMED_OUT))
    }
}

/// The one line of a failure that names it.
///
/// A red's `reason` is the assertion's sentence with the whole capture pasted
/// after it, and the capture differs between any two boots — so the first line
/// is what "the same failure" can be asked about, and it is what the summary
/// already prints.
fn headline(reason: Option<&str>) -> String {
    reason.unwrap_or("check failed").lines().next().unwrap_or("check failed").to_string()
}

/// The CPUs `test_rs_panic_halts_first` runs on: one for it and one per sibling.
const STOP_CPUS: u32 = 4;

/// **QEMU is the judge, and no clock is in it.** Once the fatal path of the
/// `test_rs_panic_halts_first` that `qemu` runs has said its line past the
/// stop, `panic_reboot::arm`'s, every vCPU but the one that went fatal must be
/// one [`qemu::stopped_cpus`] calls halted. The fatal one holds its panel, so
/// the machine is still there to ask.
fn the_others_halt_first(mut qemu: QemuInstance, arch: toyos_build::arch::Arch) -> Result<(), String> {
    let cpus = STOP_CPUS as usize;
    let mut console = String::new();
    await_guest(&mut qemu, &mut console, "the fatal path's line past the stop", |c| {
        fatal_past_the_stop(c).is_some()
    })?;
    let fatal = fatal_past_the_stop(&console).expect("awaited above");
    let before = &console[..console.find(FATAL_HALT_NONCE).expect("awaited above")];
    // Non-vacuity: another CPU was making records up to the fatal one.
    if !before.lines().any(|l| l.contains(RETIRED_RECORD) && record_cpu(l).is_some_and(|cpu| cpu != fatal)) {
        return Err(format!(
            "no other CPU made a record before the fatal one on cpu{fatal}, so nothing was running \
             to be stopped\n{console}"
        ));
    }
    let mut monitor = qemu::QmpMonitor::open(qemu.qmp_socket());
    // A guard and never a verdict: a vCPU the host has not run yet has not
    // taken the stop, and this is how long it is waited for.
    let give_up = Instant::now() + qemu::GUEST_QUIET;
    loop {
        let stopped = qemu::stopped_cpus(&mut monitor, arch);
        // QEMU's CPU#n is the kernel's cpun: its MADT lists them in that order,
        // the boot CPU first, and the kernel numbers them as the MADT lists them.
        if stopped.len() == cpus && stopped.iter().enumerate().all(|(cpu, &halted)| halted || cpu == fatal as usize) {
            break;
        }
        if Instant::now() >= give_up {
            return Err(format!(
                "{STALLED} waiting for the other CPUs to halt after the fatal path on cpu{fatal} \
                 stopped them — QEMU shows each vCPU halted with interrupts masked as {stopped:?}\n{console}"
            ));
        }
        console.push_str(&qemu.drain_serial(Duration::from_millis(200)));
    }
    eprintln!("  [panic] the fatal path on cpu{fatal} left every other CPU halted with interrupts masked");
    Ok(())
}

/// The record each sibling of `test_rs_panic_halts_first` makes, over and over.
const RETIRED_RECORD: &str = "syscall 26 is retired";

/// The CPU a kernel record is stamped with: `[kernel <secs> cpu<N>]`.
fn record_cpu(line: &str) -> Option<u32> {
    let head = line.split_once("[kernel ")?.1.split_once(']')?.0;
    head.split_once(" cpu")?.1.split(' ').next()?.parse().ok()
}

/// The CPU that went fatal, once the console carries its line past
/// `stop_other_cpus` — `panic_reboot::arm`'s, in either of its two words.
fn fatal_past_the_stop(console: &str) -> Option<u32> {
    const PAST_THE_STOP: [&str; 2] = ["panic: rebooting in", "panic: holding this panel"];
    let lines: Vec<&str> = console.lines().collect();
    let nonce = lines.iter().position(|l| l.contains(FATAL_HALT_NONCE))?;
    let fatal = record_cpu(lines[nonce])?;
    lines[nonce..]
        .iter()
        .any(|l| record_cpu(l) == Some(fatal) && PAST_THE_STOP.iter().any(|word| l.contains(word)))
        .then_some(fatal)
}

/// What a run has established, as it establishes it.
///
/// One place rather than five counters in `main`, because the interesting part
/// is not any one of them but the arithmetic between them — which reds, which
/// only reports, and what the process exits with. [`Tally::exit_code`] and
/// [`Tally::summary`] are that arithmetic, and both are gated.
struct Tally {
    passed: usize,
    failures: Vec<(String, String)>,
    /// The subset of `failures` whose ceiling expired rather than whose
    /// assertion failed, by name. Red like any other, and named apart.
    stalls: Vec<String>,
    invalid: Vec<(String, Duration)>,
}

impl Tally {
    fn new() -> Self {
        Tally { passed: 0, failures: Vec::new(), stalls: Vec::new(), invalid: Vec::new() }
    }

    fn record(&mut self, outcome: Outcome) {
        match outcome.verdict() {
            Verdict::Pass => self.passed += 1,
            Verdict::Fail => {
                if outcome.stalled() {
                    self.stalls.push(outcome.name.clone());
                }
                let said = headline(outcome.reason.as_deref());
                self.failures.push((outcome.name, said));
            }
            Verdict::Invalid => self.invalid.push((outcome.name.clone(), outcome.suspended)),
        }
    }

    /// **Three statuses**: 0 green, 1 red, and 2 when the run established
    /// nothing, because the host stopped in the middle of it.
    fn exit_code(&self) -> i32 {
        if !self.failures.is_empty() {
            return 1;
        }
        if !self.invalid.is_empty() {
            return 2;
        }
        0
    }

    /// Everything the run has to say, as one block, ending in the result line.
    ///
    /// A string rather than a pile of `eprintln!`s so that the gate can read
    /// what an agent reads.
    fn summary(&self, total: usize, elapsed: Duration, suspended: Duration) -> String {
        let mut out = String::new();
        let mut say = |line: String| {
            out.push_str(&line);
            out.push('\n');
        };
        say(String::new());
        // What the run's liveness ceilings were actually worth, because the
        // number in the source is no longer the number that was enforced. A
        // reader comparing two runs' timings needs to know which host each was
        // taken on, and this is the suite's own measurement of that.
        let (fastest, reference, num, den) = qemu::host_speed();
        if let Some(fastest) = fastest {
            say(format!(
                "host: fastest boot {fastest} ms against the reference {reference} ms — liveness \
                 ceilings paid at {:.2}x width",
                f64::from(num) / f64::from(den)
            ));
        }
        // The other half of the liveness correction is per guest, not host-wide:
        // a guest with more vCPUs than the host has cores waits `vcpus/cores`
        // longer again before its ceiling calls it wedged. Reported so a reader
        // knows whether it was ever in play — it never is once cores >= 8.
        say(format!(
            "host: {} core(s); a guest wider than that waits vcpus/cores longer again",
            qemu::host_cores()
        ));
        if suspended >= common::clock::SUSPENDED_AT_LEAST {
            // The elapsed figure below is monotonic and therefore already
            // excludes it, which is worth saying: the two numbers do not add up
            // unless a reader knows that.
            say(format!(
                "note: the host was suspended for {suspended:.0?} during this run. \
                 The suite time below excludes it."
            ));
        }
        if !self.failures.is_empty() {
            say("failures:".to_string());
            for (name, reason) in &self.failures {
                say(format!("    {name}: {reason}"));
            }
            say(String::new());
        }
        if !self.stalls.is_empty() {
            say(format!(
                "{} of those reds are the ceiling: {}",
                self.stalls.len(),
                self.stalls.join(", ")
            ));
            say(String::new());
        }
        if !self.invalid.is_empty() {
            say("invalidated by host suspend:".to_string());
            for (name, slept) in &self.invalid {
                say(format!("    {name}: the host was stopped for {slept:.0?} while it ran"));
            }
            say(String::new());
        }

        match self.exit_code() {
            1 => say(format!(
                "test result: FAILED. {} passed, {} failed, {} invalidated, \
                 {total} total ({elapsed:.1?})",
                self.passed,
                self.failures.len(),
                self.invalid.len(),
            )),
            2 => {
                say(format!(
                    "test result: INVALID. {} passed, {} invalidated by a \
                     host suspend of {suspended:.0?}, {total} total ({elapsed:.1?})",
                    self.passed,
                    self.invalid.len(),
                ));
                say(
                    "This is not a red. The machine stopped mid-run, so those verdicts \
                     are of nothing; re-run the suite."
                        .to_string(),
                );
            }
            _ => say(format!(
                "test result: ok. {} passed, {total} total ({elapsed:.1?})",
                self.passed
            )),
        }
        out
    }
}

fn run_task(task: Task, test_config: &Path, report: &std::sync::mpsc::Sender<Outcome>) {
    // Both clocks, at every test, because what the host did *between* two of
    // them is a different question from what it did during one: a lid closed
    // while nothing was running invalidates nothing.
    let name = task.name();
    let start = common::clock::mark();
    let outcome = catching(|| match task {
        Task::Machine(name) => run_machine_test(name),
        Task::Screen(name, profile) => run_screen_test(name, profile, test_config),
    });
    let _ = report.send(Outcome {
        name: name.to_string(),
        reason: outcome.err(),
        elapsed: start.elapsed(),
        suspended: start.suspended(),
    });
}

impl Task {
    /// The test this task reports an outcome for.
    fn name(&self) -> &'static str {
        match self {
            Task::Machine(name) | Task::Screen(name, _) => name,
        }
    }
}

/// Where the last run in this worktree left what each test cost it.
///
/// Under `target/`, so it is per-worktree: on a single dev host repeating runs
/// it is a *hint* about how to order a queue and never an input to a verdict,
/// where a wrong number costs some idle lane time and a missing one costs
/// nothing at all. **A sharded run does not read it** — [`shard_pricing`]
/// says why the same claim does not hold once `target/` is a cache twelve
/// separate processes restore.
fn durations_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-durations")
}

/// The profile a checkout that has never run the suite starts from.
///
/// A machine with no measurement at all prices every test the same, and
/// [`Shard::keep`]'s LPT then degenerates to round-robin — which is what put 191
/// of 268 tests on one CI shard and cut it off at its job timeout while another
/// finished in sixteen minutes. Every runner is that
/// machine on every push, because a fresh clone has no `target/`.
///
/// Measured on a runner rather than here, deliberately: it is read by the
/// machines that have nothing else, and the dev host overrides it with its own
/// numbers the first time it runs the suite. Cross-arch TCG on an M4 Pro and
/// KVM on four Azure cores do not agree about which tests are long.
fn committed_durations_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/test-durations")
}

fn read_durations(path: &Path, out: &mut BTreeMap<String, Duration>) {
    let Ok(text) = fs::read_to_string(path) else { return };
    for line in text.lines() {
        // `<label> <ms>`, read from the right.
        let Some((name, ms)) = line.rsplit_once(' ') else { continue };
        if let Ok(ms) = ms.parse() {
            out.insert(name.to_string(), Duration::from_millis(ms));
        }
    }
}

/// The committed profile, with whatever this worktree has measured on top.
///
/// Per name rather than per file, so a checkout that has only ever run a filter
/// keeps the committed number for everything that filter did not name.
fn load_durations() -> BTreeMap<String, Duration> {
    let mut out = BTreeMap::new();
    read_durations(&committed_durations_path(), &mut out);
    read_durations(&durations_path(), &mut out);
    out
}

/// What [`longest_first`] and [`Shard::keep`] price a task against.
///
/// **Committed only — never [`durations_path`]'s worktree overlay.** That
/// overlay lives under `target/`, which each shard restores from a build cache
/// on its own, so two shards need not read one overlay. `Shard::keep` assumes
/// every process prices a task identically, and two disagreeing on one number
/// is one test run twice and another nowhere. `tests/test-durations` is
/// `actions/checkout`, not `actions/cache`, and every shard checks out the
/// identical bytes.
fn shard_pricing() -> BTreeMap<String, Duration> {
    let mut out = BTreeMap::new();
    read_durations(&committed_durations_path(), &mut out);
    out
}

/// Merge this run's durations into the recorded profile.
///
/// Merged rather than replaced, because a filtered run knows about four tests
/// and would otherwise throw away what the last full one measured. A sharded
/// run never calls this: the partition is a function of the profile, so a
/// shard that saved would move it under its siblings.
fn save_durations(mut known: BTreeMap<String, Duration>, timed: &[(String, Duration)]) {
    for (name, elapsed) in timed {
        known.insert(name.clone(), *elapsed);
    }
    let path = durations_path();
    let body = known.iter().map(|(n, d)| format!("{n} {}\n", d.as_millis())).collect::<String>();
    let tmp = path.with_extension("tmp");
    if fs::create_dir_all(path.parent().expect("target/ has a parent")).is_ok()
        && fs::write(&tmp, body).is_ok()
    {
        let _ = fs::rename(&tmp, &path);
    }
}

/// Longest job first, on what the last run measured.
///
/// A phase's wall clock is `max(sum / width, longest job)`, and FIFO reaches the
/// first term only if no long job is dispatched late. Declaration order puts the
/// feature-carrying tests last — deliberately, to keep the kernel rebuilds
/// together — which is exactly the worst order for a wide phase.
///
/// **The profile is measured, not declared**, because the alternative is a
/// hand-maintained list of long tests — a second registration to keep true, and
/// one nothing would notice going stale. A name the file has never seen sorts
/// first, so a new test is assumed long until it has been timed once: the cost of
/// being wrong that way is one lane starting a short job early.
fn longest_first(tasks: &mut [Task], known: &BTreeMap<String, Duration>) {
    let cost = |task: &Task| -> Duration { known.get(task.name()).copied().unwrap_or(Duration::MAX) };
    tasks.sort_by_key(|task| std::cmp::Reverse(cost(task)));
}

/// One outcome, as the run prints it.
fn report_line(outcome: &Outcome) {
    let reason = || outcome.reason.as_deref().unwrap_or("check failed");
    match outcome.verdict() {
        Verdict::Pass => eprintln!("  PASS  {}  ({:.0?})", outcome.name, outcome.elapsed),
        Verdict::Fail => {
            eprintln!("FAIL {}: {}", outcome.name, reason());
            if outcome.stalled() {
                eprintln!(
                    "  STALL {}  ({:.0?})",
                    outcome.name, outcome.elapsed
                );
            } else {
                eprintln!("  FAIL  {}  ({:.0?})", outcome.name, outcome.elapsed);
            }
        }
        Verdict::Invalid => eprintln!(
            "  INVL  {}  ({:.0?}) — the host was suspended for {:.0?} while it ran",
            outcome.name, outcome.elapsed, outcome.suspended
        ),
    }
}

/// Run `tasks` on `width` workers, printing each outcome as it lands.
///
/// One implementation for both phases: **the serial tail is this at width 1**,
/// so "serial" is a number rather than a second code path that could drift from
/// this one. It returns only once every worker has joined, which is what makes
/// "the parallel phase has drained" a fact about the call and not about where
/// it sits in `main`.
fn run_phase(tasks: Vec<Task>, width: usize, test_config: &Path) -> Vec<Outcome> {
    if tasks.is_empty() {
        return Vec::new();
    }
    let width = width.clamp(1, tasks.len());
    qemu::set_width(width as u32);
    let queue = std::sync::Mutex::new(std::collections::VecDeque::from(tasks));
    let mut all = Vec::new();
    thread::scope(|scope| {
        let (tx, rx) = std::sync::mpsc::channel::<Outcome>();
        for lane in 0..width {
            let tx = tx.clone();
            let queue = &queue;
            scope.spawn(move || {
                common::lane::enter(lane);
                loop {
                    let next =
                        queue.lock().expect("a worker panicked holding the queue").pop_front();
                    let Some(task) = next else { return };
                    run_task(task, test_config, &tx);
                }
            });
        }
        drop(tx);
        for outcome in rx {
            report_line(&outcome);
            all.push(outcome);
        }
    });
    all
}

/// Every test with a boot, split into the parallel and serial phases.
///
/// Pulled out of `main` so [`check_shard_partition`] builds the identical
/// lists a real run would rather than a second, hand-written approximation
/// that could pass its own check while the real path still disagreed with
/// itself — which is exactly the shape of the defect run `31617589126` found.
fn build_tasks(
    machine_to_run: &[(&'static str, Sched)],
    screen_to_run: &[(&'static str, Sched, qemu::Profile)],
) -> (Vec<Task>, Vec<Task>) {
    let mut parallel: Vec<Task> = Vec::new();
    let mut serial: Vec<Task> = Vec::new();
    let machine = machine_to_run.iter().map(|&(name, sched)| (Task::Machine(name), sched));
    let screen =
        screen_to_run.iter().map(|&(name, sched, profile)| (Task::Screen(name, profile), sched));
    for (task, sched) in machine.chain(screen) {
        match sched {
            Sched::Parallel => parallel.push(task),
            Sched::Serial => serial.push(task),
        }
    }
    (parallel, serial)
}

/// Whether a run takes `name`: its filter matches it and no redlist row
/// disables it.
fn kept(filter: Option<&str>, name: &str) -> bool {
    filter.is_none_or(|f| name.contains(f)) && redlist::disabled(redlist::DISABLED, name).is_none()
}

/// The machine tests and the screen tests a run boots.
type Selection = (Vec<(&'static str, Sched)>, Vec<(&'static str, Sched, qemu::Profile)>);

/// Every declared test a run [`kept`], but on a shard only the screen rows
/// whose profile is of [`toyos_build::ci::GUEST_ARCH`].
fn select(filter: Option<&str>, sharded: bool) -> Selection {
    (
        MACHINE_TESTS.iter().filter(|(n, _)| kept(filter, n)).copied().collect(),
        SCREEN_TESTS
            .iter()
            .filter(|(n, _, profile)| {
                kept(filter, n) && (!sharded || profile.arch() == toyos_build::ci::GUEST_ARCH)
            })
            .copied()
            .collect(),
    )
}

fn arch_drop_line(filter: Option<&str>) -> Option<String> {
    let (_, whole) = select(filter, false);
    let (_, shard) = select(filter, true);
    let dropped: Vec<&str> = whole
        .iter()
        .map(|(name, _, _)| *name)
        .filter(|name| !shard.iter().any(|(kept, _, _)| kept == name))
        .collect();
    (!dropped.is_empty()).then(|| {
        format!(
            "{} test(s) NOT run, because a shard boots no guest but {}: {}",
            dropped.len(),
            toyos_build::ci::GUEST_ARCH.name(),
            dropped.join(", ")
        )
    })
}

/// **The property every merged CI run depends on, checked before any of the
/// twelve processes that would otherwise each discover it separately.** Every
/// name [`Shard::keep`] is handed for `count` must land in exactly one of
/// `1..=count`'s shards: a violation is one test run twice and another
/// nowhere, with every shard green.
///
/// This cannot reproduce *why* two real processes disagreed — that needs
/// [`shard_pricing`]'s fix, not a test, because the defect was two machines
/// pricing a task from two different `target/test-durations` a shared build
/// cache handed them. What this can and does check is the part a shared-fate
/// bug would otherwise hide behind: that pricing every task from the
/// committed profile alone — the one input every process is guaranteed to
/// agree on — still yields a clean partition, for real registration data, at
/// the width CI actually runs.
fn check_shard_partition() {
    let pricing = shard_pricing();
    let (machine_to_run, screen_to_run) = select(None, true);
    let (parallel, serial) = build_tasks(&machine_to_run, &screen_to_run);
    let want: BTreeSet<&str> = parallel.iter().chain(&serial).map(Task::name).collect();

    const COUNT: usize = 12;
    let cost = |task: &Task| -> Option<Duration> { pricing.get(task.name()).copied() };
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for index in 1..=COUNT {
        let shard = Shard { index, count: COUNT };
        let mut mine_p = parallel.clone();
        let mut mine_s = serial.clone();
        // One accumulator across the two pools, in the order `main` takes
        // them: the partition a shard gets is a function of both calls, so a
        // check that took them apart would be checking something else.
        let mut load = shard.bins();
        shard.keep(&mut mine_p, &mut load, cost);
        shard.keep(&mut mine_s, &mut load, cost);
        for name in mine_p.iter().chain(&mine_s).map(Task::name) {
            assert!(
                seen.insert(name),
                "{name} lands in shard {index}/{COUNT} and at least one earlier shard too — \
                 every execution label must belong to exactly one"
            );
        }
    }
    assert_eq!(
        seen, want,
        "the twelve shards together do not equal the full selection — {:?} present in the \
         selection and missing from every shard",
        want.difference(&seen).collect::<Vec<_>>()
    );
}

/// Every claim the metal table makes about itself, before anything boots:
/// every row is its test's one declaration and asks for at least one boot, and
/// every boot it asks for is a committed config.
fn check_registration() {
    if let Err(why) = the_two_comparisons_use_one_rule() {
        panic!("{why}");
    }
    for (case, _) in C_METAL_SKIP {
        let at = compile::testcases_dir().join(format!("{case}.c"));
        assert!(
            at.is_file(),
            "C_METAL_SKIP names {case:?}, which the corpus does not hold; a row for a case \
             that is gone excludes nothing and hides that it is gone"
        );
    }
    devices::the_config_runs_exactly_these_jobs();
    if let Err(why) = the_metal_gate_refuses_what_it_names() {
        panic!("{why}");
    }
    if let Err(why) = metal_rows_are_whole(METAL) {
        panic!("{why}");
    }
}

/// [`check_registration`]'s rule over any table, so the gate is held to
/// fixtures as well as to the table it guards.
fn metal_rows_are_whole(rows: &[(&str, metal::Metal)]) -> Result<(), String> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (name, decl) in rows {
        if !seen.insert(name) {
            return Err(format!("{name} has two metal declarations"));
        }
        if decl.arms.is_empty() {
            return Err(format!("{name}'s metal declaration asks for no boot at all"));
        }
        for arm in decl.arms {
            let at = compile::repo_root().join(arm.config).join("system.toml");
            if !at.is_file() {
                return Err(format!("{name} boots {}, which holds no system.toml", arm.config));
            }
        }
    }
    Ok(())
}

/// The metal gate, on fixtures: every refusal it names is shown to fire, and
/// the table it must accept is accepted.
fn the_metal_gate_refuses_what_it_names() -> Result<(), String> {
    fn judge(_: &[&metal::Readback]) -> Result<(), String> {
        Ok(())
    }
    const RUNS: metal::Metal = metal::Metal { arms: JOBCASE, judge };
    const NONE: metal::Metal = metal::Metal { arms: &[], judge };
    const NO_CONFIG: metal::Metal = metal::Metal {
        arms: &[metal::once("nowhere", "tests/no-such-config", &[], &[])],
        judge,
    };
    /// A fixture's name, its METAL rows, and the refusal it must draw, `None`
    /// for none.
    type Case = (&'static str, &'static [(&'static str, metal::Metal)], Option<&'static str>);
    let cases: &[Case] = &[
        ("two rows", &[("one", RUNS), ("two", RUNS)], None),
        ("a row twice", &[("one", RUNS), ("one", RUNS)], Some("has two metal declarations")),
        ("no boot", &[("one", NONE)], Some("asks for no boot at all")),
        ("no config", &[("one", NO_CONFIG)], Some("holds no system.toml")),
    ];
    for (case, rows, refused) in cases {
        match (metal_rows_are_whole(rows), refused) {
            (Ok(()), None) => {}
            (Err(got), Some(want)) if got.contains(want) => {}
            (got, _) => {
                return Err(format!(
                    "the metal registration gate on {case} answered {got:?}, and it has to \
                     answer {}",
                    refused.map_or("Ok".to_string(), |w| format!("a refusal saying {w:?}"))
                ))
            }
        }
    }
    Ok(())
}

/// Every name the two declared registries give.
fn declared<'a>() -> impl Iterator<Item = &'a str> {
    MACHINE_TESTS.iter().map(|(n, _)| *n).chain(SCREEN_TESTS.iter().map(|(n, _, _)| *n))
}

/// What the shared boots answer for: every discovered Rust binary and every
/// corpus case.
fn shared_names() -> Vec<String> {
    discover_rust_tests().into_iter().chain(discover_c_tests()).collect()
}

/// Every name this suite can produce a verdict for, on either machine: the
/// shared boots' `shared` names, the declared registries and the metal table,
/// so a name two of them give is refused before anything boots.
fn registered(shared: &[String]) -> Result<BTreeSet<&str>, String> {
    let mut names = BTreeSet::new();
    let tables = declared().chain(METAL.iter().map(|(n, _)| *n));
    for name in shared.iter().map(String::as_str).chain(tables) {
        if !names.insert(name) {
            return Err(format!(
                "{name} is registered twice, and two rows are two verdicts under one name. A \
                 binary a metal row drives goes on RUST_SKIP with the reason its own row \
                 exists, or one of the two is renamed."
            ));
        }
    }
    Ok(names)
}

/// `redlist::DISABLED` against every registered name, before any boot on any
/// entry point.
fn check_redlist(registered: &BTreeSet<&str>) -> Result<(), String> {
    redlist::check(redlist::DISABLED, |name| registered.contains(name), &compile::repo_root())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // First, before any lock and before anything is compiled: a flag this suite
    // does not have would otherwise cost nothing and hand its value to the
    // filter below.
    let parsed = match toyos_build::testargs::parse(&args) {
        Ok(parsed) => parsed,
        Err(refusal) => {
            eprintln!("[toyos] {refusal}");
            std::process::exit(1);
        }
    };
    let filter = parsed.filter;
    // The one selection every entry point below takes, the metal's included: a
    // disabled test runs nowhere, and every run names each one with its issue.
    for row in redlist::DISABLED {
        eprintln!("[toyos] disabled: {} — {}", row.test, row.issue);
    }
    let keep = |name: &str| kept(filter, name);

    let debug_mode = SUITE.present(&args, &testargs::DEBUG);
    let list_mode = SUITE.present(&args, &testargs::LIST);
    let nocapture = SUITE.present(&args, &testargs::NOCAPTURE);

    // How many guests the parallel phase runs at once. The serial tail ignores
    // it — that is what it is.
    let width = SUITE
        .value(&args, &testargs::JOBS)
        .or_else(|| SUITE.value(&args, &testargs::JOBS_SHORT))
        .map_or(DEFAULT_WIDTH, |n| {
            let width: usize = n.parse().unwrap_or_else(|_| panic!("--jobs: {n:?} is not a width"));
            assert!(width >= 1, "--jobs needs at least one worker");
            width
        });

    // Which slice of the suite this machine runs. Absent is the whole of it.
    let shard = match testargs::parse_shard(&args) {
        Ok(shard) => shard,
        Err(refusal) => {
            eprintln!("[toyos] {refusal}");
            std::process::exit(1);
        }
    };

    // Before anything boots: every exit below goes through `run`, which removes
    // this run's scratch, green or red; taking it reclaims what killed runs left.
    let run = common::lane::Run::begin();

    check_registration();

    if nocapture || debug_mode {
        common::qemu::VERBOSE.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // Every name this process could produce a verdict for, on either machine,
    // before `--list`, `--debug` or `--metal` can return without reaching it.
    let shared = shared_names();
    let registered = match registered(&shared) {
        Ok(registered) => registered,
        Err(refusal) => {
            eprintln!("[toyos] {refusal}");
            run.exit(1);
        }
    };
    if let Err(refusal) = check_redlist(&registered) {
        eprintln!("[toyos] src/redlist.rs: {refusal}");
        run.exit(1);
    }

    if list_mode {
        for name in &registered {
            println!("{name}");
        }
        return;
    }

    if let Some(mode) = parsed.metal {
        let (c_bins, rust_bins) = build_shared_bins();
        let selected: Vec<(&str, &'static metal::Metal)> = METAL
            .iter()
            .filter(|(name, _)| keep(name))
            .map(|(name, decl)| (*name, decl))
            .collect();
        // The shared boots carry names no registration holds, so an empty
        // selection is only a dead filter when they are empty too — which
        // `metal::run` says for itself.
        if selected.is_empty() && filter.is_some() {
            eprintln!(
                "[toyos] no metal registration matches filter {filter:?}; the shared boots' \
                 members are not filtered by name"
            );
        }
        let mut boots = shared_metal(keep);
        boots.push(c_corpus_metal(&c_bins, keep));

        // Three statuses for the three things this can establish, as the
        // ordinary suite has: green, red, and "measured nothing" — a run that
        // staged images and never reached the machine has no claim to make.
        run.exit(
            match metal::run(mode, &selected, &boots, &rust_bins, !nocapture && !debug_mode)
            {
                metal::Verdict::Green => 0,
                metal::Verdict::Red => 1,
                metal::Verdict::Staged => 2,
            },
        );
    }

    if debug_mode {
        let (c_bins, rust_bins) = build_shared_bins();
        run_debug_mode(&c_bins, &rust_bins);
        return;
    }

    check_shard_partition();

    let (machine_to_run, screen_to_run) = select(filter, shard.is_some());

    if shard.is_some() {
        if let Some(line) = arch_drop_line(filter) {
            eprintln!("[toyos] {line}");
        }
    }

    if screen_to_run.is_empty() && machine_to_run.is_empty() {
        eprintln!("No enabled test matches filter {filter:?}");
        run.exit(1);
    }

    let test_config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testcases");
    let mut tally = Tally::new();

    let suite_start = common::clock::mark();

    let (mut parallel, mut serial) = build_tasks(&machine_to_run, &screen_to_run);

    let known = load_durations();
    // After the phases are decided and before either is ordered: what a shard
    // divides is the work, and a task's answer to `Sched` is a property of the
    // test rather than of how many machines are running it.
    if let Some(shard) = shard {
        // [`shard_pricing`], and not `known`: every process partitioning the
        // same run must price a task identically, which only the committed
        // profile guarantees.
        let pricing = shard_pricing();
        // A task the profile has never timed is unmeasured, which is the rule
        // [`longest_first`] states with `Duration::MAX`.
        let cost = |task: &Task| -> Option<Duration> { pricing.get(task.name()).copied() };
        longest_first(&mut parallel, &pricing);
        longest_first(&mut serial, &pricing);
        // One accumulator for the whole run, heaviest pool first: this process
        // runs both pools one after another, so its wall clock is the one bin
        // they share and the serial tail belongs in whichever bin the parallel
        // phase left lightest. Two partitions from two empty accumulators are
        // each good and their sum is not
        // (`Shard::keep`, and run `31377439504`'s 466.1 s against a 369.1 s
        // even split).
        let mut load = shard.bins();
        shard.keep(&mut parallel, &mut load, cost);
        shard.keep(&mut serial, &mut load, cost);
        eprintln!(
            "[toyos] shard {}/{}: {} parallel task(s), {} serial",
            shard.index,
            shard.count,
            parallel.len(),
            serial.len(),
        );
    }

    // Counted from the task lists rather than from the filtered ones, because a
    // shard's own total is what its summary has to add up against.
    let total = parallel.len() + serial.len();
    if let Err(refusal) = toyos_build::testargs::validate_ordinary_shard(shard, filter, total) {
        eprintln!("[toyos] {refusal}");
        run.exit(1);
    }
    eprintln!("\nrunning {total} tests\n");

    let mut timed: Vec<(String, Duration)> = Vec::new();
    if !parallel.is_empty() {
        longest_first(&mut parallel, &known);
        eprintln!("  --- parallel, {width} wide ---");
        let started = std::time::Instant::now();
        let outcomes = run_phase(parallel, width, &test_config);
        eprintln!("  --- parallel done in {:.1?} ---", started.elapsed());
        timed.extend(outcomes.iter().map(|o| (o.name.clone(), o.elapsed)));
        outcomes.into_iter().for_each(|o| tally.record(o));
    }
    if !serial.is_empty() {
        eprintln!("  --- serial ---");
        let started = std::time::Instant::now();
        let outcomes = run_phase(serial, 1, &test_config);
        eprintln!("  --- serial done in {:.1?} ---", started.elapsed());
        timed.extend(outcomes.iter().map(|o| (o.name.clone(), o.elapsed)));
        outcomes.into_iter().for_each(|o| tally.record(o));
    }
    if shard.is_none() {
        save_durations(known, &timed);
    }

    // Three exit statuses, because there are three things a run can establish —
    // see [`Tally::exit_code`], which is where the whole decision now lives.
    //
    // Nor may it be 1. A red sends an agent hunting a defect, and the defect is
    // not there — the lid was closed. CLAUDE.md already documents the signature
    // and documents it as something a *human* must notice before recording a
    // finding, which is exactly the judgement a status code should carry
    // instead. So: 2, with a headline that names it and says re-run.
    //
    // A run with both real failures and invalidated tests exits 1: a red that
    // survives is still a red, and re-running the suspended ones does not make
    // it green.
    // What this run cost cargo: a kernel build is ~6.9 s of wall clock and
    // ~29.6 s of CPU after any edit under `kernel/`, and a full run used to
    // make 45 of them.
    let (boots, feature_boots, kernels) = qemu::boot_census();
    eprintln!(
        "  --- {boots} guests, {feature_boots} of them not the shipping kernel, {} kernel \
         build(s): {kernels:?}",
        kernels.len(),
    );

    // Where this run's interrupts landed, aggregated over every guest that
    // said. `issues/kernel/every-interrupt-lands-on-the-boot-cpu.md`'s step 4:
    // the number its later change is measured against, produced by an ordinary
    // run rather than by `--nocapture`, so a CI shard's own log carries it.
    eprint!("{}", common::irqcensus::summary());

    eprint!("{}", tally.summary(total, suite_start.elapsed(), suite_start.suspended()));
    run.exit(tally.exit_code());
}

