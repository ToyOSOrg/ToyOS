#[macro_use(eprintln)]
extern crate toyos_build;

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
use common::{audio, claims, compile, devices, faults, isa, metal, power, screen, serial, usb};
use toyos_build::bootlog::{self};
use toyos_build::testargs::{self, SUITE};

/// The width with no `--jobs`.
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
    // Action 16, the live-object census per kind. A leak is two readings and a
    // comparison, so on a kernel that answers `InvalidArgument` both readings
    // are the same error and the assertion passes having counted nothing.
    // Both took action 16 in place of `SYS_SYSINFO`: a verdict about
    // what one killed process gave back cannot be the whole machine's free
    // memory, which every other binary in a shared boot moves under it.
    "handle_lifetime",
    "shm_release_reclaims",
    // Action 22: the kernel kills a spawn's place between the spawn's commit
    // and its landing, a window no caller can order a kill inside.
    "spawn_lands_claimed",
    // Action 23: the kernel holds a spawn, once its child has landed, until
    // the child has ended — a child's end no caller can order inside its spawn.
    "spawn_child_ends_first",
    // Actions 24 and 25: one CPU answers no counters round until it is heard
    // again, which is what a CPU silent past a read's bound is to the reader,
    // and nothing in a guest makes one on demand.
    "counters_silent",
    // Action 26: the timer's interrupt inside a syscall's body, which only a
    // running gate decides and nothing in a guest puts there on demand.
    "ring0_timer_in_syscall",
];

/// What [`ACTUATOR_TESTS`] boots: the one kernel that carries `SYS_DEBUG`.
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
    // Its product is the windows a `mask-windows` kernel reports under it, and
    // its 256 threads' stacks would crowd every member after it in one boot.
    // The `mask_windows` metal row runs it.
    "ring_park_herd",
    // It asserts nothing: it is half a second of an idle machine, which the
    // same row reads the windows across.
    "idle_span",
    // The C corpus's comparator: a helper reached through one symlink per case,
    // never a test of its own. `shared_metal` stages every name on this list.
    "ccheck",
    "disk_backtrace_child",
    "fault_gate_child",
    // Each needs a boot whose i8042 the kernel does not drive: the
    // `isa_ports_are_the_binders_alone` and `isa_lines_reach_their_holder`
    // metal rows run them.
    "isa_grant",
    "isa_lines",
    // It claims the T14's I219, which no guest has: the
    // `claim_reuses_its_remapping_entry` and `claim_refused_without_remapping`
    // metal rows run it.
    "pci_reclaim",
    // Its product is the T14's counters across an idle span and a spin on
    // every CPU, which the `counters` metal row judges; on a guest it would be
    // seconds of four CPUs spinning, read by nothing.
    "counters_metal",
    // Its product is what a diary record costs, which the `trace_record_cost`
    // metal row reads off the test kernel its flood needs.
    "trace_flood",
    // It takes the machine down; `virt_fatal_halts_the_others_first` runs it.
    "panic_halts_first",
    // Needs a launcher and a declared `cat` and shell, which `tests/testcases`
    // does not give: the `process_tree` metal row runs it on tests/proctreecase.
    "process_tree",
    // Needs a launcher and a declared `toybox` holding `roster`, which
    // `tests/testcases` does not give: the `launch_toctou` metal row runs it on
    // tests/proctreecase.
    "launch_toctou",
    // Needs a launcher whose row lists `swap` and `update` and not `proctest`,
    // and a shell that opens a login session: the `launch_authority` metal row
    // runs it on tests/proctreecase.
    "launch_authority",
    // A kernel primitive with no use for any one boot's devices: the
    // `port_badge` metal row runs it on tests/proctreecase.
    "port_badge",
    // Needs a launcher whose row lists a shell, and a shell whose row opens a
    // login session and lists a shell, so that a login session and its
    // launches ask DATA's server while this job's share holds all it may: the
    // `fs_share` metal row runs it on tests/proctreecase.
    "fs_share",
    // It needs a NIC in front of netstack and a host server behind it:
    // `netstack_socket_churn` runs it on `tests/netcase`.
    "netstack_socket_churn",
    // It asserts nothing at all: it holds `dump_nmi_probe`'s boot open for
    // twenty seconds. On a shared boot it would be twenty seconds of nothing.
    "lan_hold",
    // Fills /tmp to the VFS listing limit, so it needs a boot nothing else
    // shares — every later `read_dir("/tmp")` in it would be refused.
    // `readdir_bound` gives it one.
    "readdir_bound",
    // Fills the VFS `created_dirs` cap and leaves it there. `mkdir_cap` runs it.
    "mkdir_cap",
    // Each fills a bound of its own process — tens of thousands of mappings,
    // thousands of threads, a thousand 2 MiB images — which no shared member's
    // allowance is sized for: the `process_bound_*` metal rows run them.
    "abuse_mmap_regions",
    "abuse_thread_table",
    "abuse_dlopen_ledger",
    // Audio is judged on the T14 and nowhere else: the `hda_client_stall`,
    // `hda_tone`, `audio_idle_suspend`, `shipped_client_departures` and
    // `soundserver_log_stall` metal rows run these.
    "hda_client_stall",
    "audio_tone",
    "audio_idle_suspend",
    "null_sink_client_exits",
    "soundserver_log_stall",
    // It hands the ACPI claim to a server of its own and kills it, which needs
    // a boot that starts none: the `acpi_server_death` metal row runs it on
    // tests/acpicase.
    "acpi_release",
    // It waits for the ACPI server's count of its embedded controller's
    // queries, and no guest's machine has that controller: the
    // `acpi_server_events` metal row runs it.
    "acpi_hold",
    // It claims the fixed hardware itself, which needs a boot that starts no
    // server, and stages the firmware's side of the Global Lock and finds the
    // i8042's row another claim's, which need the test kernel and its
    // `i8042-withheld`: `acpi_mediated_access` runs it on tests/acpicase.
    "acpi_mediated",
    // It powers the machine off: `machine_shutdown_short_stop` runs it.
    "stop_short",
    // It claims QEMU's virtio NIC, which the T14 has none of: `bar_map_again`
    // runs it.
    "bar_map_again",
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
    // Its shared run is the x86-64 verdict; `virt_smp` builds it for AArch64
    // and runs it on that architecture's SMP case.
    "counters_read",
    "trace_read",
    // Its shared run is the x86-64 verdict; `virt_readonly_copyout` builds it
    // for AArch64 and runs it on that architecture's job case.
    "abuse_readonly_copyout",
    // Its actuator-boot run is the T14's verdict; `virt_ring0_timer_in_syscall`
    // builds it for AArch64 and runs it on that architecture's job case.
    "ring0_timer_in_syscall",
    // Its shared run asserts every arm's kill; `crash_report_reads_no_kernel_memory`
    // reads what the kernel said of two of them.
    "fault_gates",
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
const SCREEN_TESTS: &[(&str, qemu::Profile)] = &[
    ("screen_panic_muted", qemu::Profile::Metal),
    // The same fatal path from inside Ctrl+Alt+D's report painter, holding the
    // panel's latch it will never give back: the report has to take the screen
    // anyway, and its CPU has to go on to watch the reset bound. The profile
    // whose 16550 is the console, where the fatal path writes its last line raw.
    ("screen_fatal_behind_a_painter", qemu::Profile::Metal),
    // The same fatal path with a compositor holding the panel, which is the
    // only configuration the owner's laptop is ever in.
    ("screen_fatal_halt_composited", qemu::Profile::Metal),
    // What the loader leaves on the panel: its own lines, and none of the
    // firmware's.
    ("screen_loader_clears", qemu::Profile::Metal),
    ("virt_early_panic", qemu::Profile::Virt),
    ("virt_early_fault", qemu::Profile::Virt),
    ("virt_el2_drop", qemu::Profile::VirtEl2NoVhe),
    ("virt_user_mode", qemu::Profile::VirtEl2),
    ("virt_timer_preempts", qemu::Profile::VirtEl2),
    ("virt_irq_storm", qemu::Profile::VirtEl2),
    ("virt_timer_floor", qemu::Profile::VirtEl2),
    ("virt_fp_isolation", qemu::Profile::VirtEl2),
    ("virt_first_entry", qemu::Profile::VirtEl2),
    ("virt_unmap_touch", qemu::Profile::VirtEl2),
    ("virt_debug_refused", qemu::Profile::VirtEl2),
    ("virt_readonly_copyout", qemu::Profile::VirtEl2),
    ("virt_ring0_timer_in_syscall", qemu::Profile::VirtEl2),
    ("virt_mask_windows", qemu::Profile::VirtEl2),
    ("virt_smp", qemu::Profile::VirtEl2),
    ("virt_el1_smp", qemu::Profile::VirtTcg),
    ("virt_failed_ap_leaves_no_hole", qemu::Profile::VirtEl2),
    ("virt_fatal_halts_the_others_first", qemu::Profile::VirtEl2),
    ("virt_reboot", qemu::Profile::VirtEl2),
    ("virt_off_names_the_cpus_left_on", qemu::Profile::VirtEl2),
    ("virt_reboot_refused_without_psci", qemu::Profile::VirtEl2),
];

/// The tests whose machine shape *is* the test, each on a boot of its own.
/// `run_machine_test` dispatches them.
const MACHINE_TESTS: &[&str] = &[
    // Whether QEMU's virtio functions negotiated `VIRTIO_F_ACCESS_PLATFORM`
    // behind its emulated VT-d unit and without one: no shipped machine has a
    // virtio function, so only a QEMU machine can be asked.
    "iommu_virtio_platform",
    // netstack's own state behind a real stack and a peer that ends its
    // connections: netstack is one binary that owns its NIC, with no host
    // build, and the T14's peer is the bench's network.
    "netstack_socket_churn",
    // What libc's socket calls ask of netstack, read back from a peer that
    // answers: the calls are libc's requests on netstack's port, netstack has
    // no host build, and the T14's peer is the bench's network.
    "libc_sockets",
    // The nested-NMI report is a raw write to the 16550, which the T14 does not
    // have.
    "nested_nmi_is_loud",
    // The power-off itself: the metal loop reaches the T14 over `ssh` and has
    // no way to turn it back on, so only a machine QEMU reports stopping can
    // be asked. `acpi_tables_loaded` reads the sleep type the T14's server
    // handed its kernel.
    "machine_shutdown",
    // The press itself: QEMU raises the fixed power-button event on demand,
    // and nothing presses the T14's button but a hand.
    "acpi_power_button",
    // What the kernel reads and writes for the `acpi` claim's holder and what
    // it refuses. A red here is a write the kernel made — to RAM, to the
    // firmware's tables, to COM1, to `PM1a_CNT`, to a function's configuration
    // space — and the take that finds the Global Lock owned and the release
    // that owes `GBL_RLS` need the FACS's word staged as only an idle firmware
    // allows: neither is done to the T14, which nothing powers on again.
    "acpi_mediated_access",
    // The same boot on one CPU, where the power-off's CPU is the only one
    // there is: what it logs reaches the console by the stop's own drain or
    // not at all, since no other CPU runs `klogd` beside it.
    "acpi_lock_given_back_on_one_cpu",
    // A power-off on the sleep type of a holder that is gone: the kernel's
    // static across a claim's release, and a machine that answers the write
    // by stopping. No host test reaches either, and the T14 is never asked to
    // power off.
    "acpi_supply_outlives_holder",
    // The power-off after a stop that left a thread running, in ACPI mode: it
    // ends the machine, so only one QEMU reports stopping can be asked, and
    // the T14 hands over in legacy mode, where no holder means no quieting.
    "machine_shutdown_short_stop",
    // A claimable function with a mappable BAR that no program of the boot
    // holds: QEMU's virtio NIC on `tests/testcases`. The T14's one such
    // function is its I219, and a kernel that dies on this takes the bench's
    // machine down with it.
    "bar_map_again",
    // `console/system.toml`'s image, which runs no job and hands no machine
    // back: a metal boot of it ends with a hand on the power button.
    "console_image_boots",
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
    // ---- one image: tests/testcases, no parameters, one job list ----
    (
        "blackbox_unclaimed_page",
        metal::Metal { arms: TESTCASES, judge: |b| power::blackbox_unclaimed(&b[0].loader(), &b[0].kernel()) },
    ),
    (
        // The machine's own CPU count, off the SMP bring-up records — a source
        // independent of the `control_regs:` lines it is then held to.
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
        "irq_census_conservation",
        // Off the page: the stop takes the boot's one census after
        // `logkeeper` has stopped, so no file carries it.
        metal::Metal {
            arms: TESTCASES,
            judge: |b| irq_census(b[0].after_the_reset()?.text()),
        },
    ),
    (
        // The windows on the machine that owes them: every CPU in every
        // report, and a held window read back.
        "mask_windows",
        metal::Metal { arms: WINDOWSCASE, judge: |b| windows_on_metal(b[0]) },
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
        "process_bound_regions",
        metal::Metal { arms: BOUNDS, judge: |b| b[0].job_passed("test_rs_abuse_mmap_regions") },
    ),
    (
        "process_bound_threads",
        metal::Metal { arms: BOUNDS, judge: |b| b[0].job_passed("test_rs_abuse_thread_table") },
    ),
    (
        "process_bound_libraries",
        metal::Metal { arms: BOUNDS, judge: |b| b[0].job_passed("test_rs_abuse_dlopen_ledger") },
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
        // exits 0, and soundserver names how each left.
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
        // soundserver with no client costs no CPU, before any client has connected.
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
        "soundserver_log_stall",
        metal::Metal {
            arms: LOGSTALLCASE,
            judge: |b| {
                b[0].job_passed("test_rs_soundserver_log_stall")?;
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
        // Every CPU's counters as the shipped syscall reads them, across an
        // idle second and a spin on every CPU: `counters_on_metal` says what
        // is held and what is read beside Linux's.
        "counters",
        metal::Metal {
            arms: &[metal::once("testcases", "tests/testcases", &[], &["test_rs_counters_metal"])],
            judge: |b| counters_on_metal(b[0]),
        },
    ),
    (
        // The ACPI server on the machine its stage is for: the row the kernel
        // filled from the T14's own tables, the server armed on it, and the
        // embedded controller's events taken and counted, on a boot held open
        // past the server's count interval.
        "acpi_server_events",
        metal::Metal {
            arms: TESTCASES_HELD,
            judge: |b| acpi_events_on_metal(b[0]),
        },
    ),
    (
        // The server's load of the T14's own definition blocks, through the
        // kernel's mediated access: `acpi_tables_on_metal` says what is read.
        // The same boot as `acpi_server_events`.
        "acpi_tables_loaded",
        metal::Metal {
            arms: TESTCASES_HELD,
            judge: |b| acpi_tables_on_metal(b[0]),
        },
    ),
    (
        // The server killed: the kernel writes `ACPI_DISABLE` as its claim goes,
        // and `SCI_EN` reads clear after it.
        "acpi_server_death",
        metal::Metal {
            arms: &[metal::once("acpicase", "tests/acpicase", &[], &["test_rs_acpi_release"])],
            judge: |b| acpi_death_on_metal(b[0]),
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
        // What one diary record costs its writer: a million written back to
        // back by `SYS_DEBUG`'s flood, so the kernel that carries it, timed
        // with interrupts closed. Read, not held: the number is the product.
        "trace_record_cost",
        metal::Metal {
            arms: &[metal::Arm {
                features: toyos_build::build::TEST_KERNEL,
                ..metal::once("testcases-debug", "tests/testcases", &[], &["test_rs_trace_flood"])
            }],
            judge: |b| {
                b[0].job_passed("test_rs_trace_flood")?;
                let log = b[0].log();
                eprintln!("  [trace] {}", log.must_say("trace_flood: ")?);
                Ok(())
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
            // off a number the harness staged.
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
    // ---- tests/jobcase, each image armed to end its boot its own way ----
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
    // ---- one image: tests/proctreecase ----
    (
        // A parent's end takes its children down. The guest carries every
        // verdict but two the kernel speaks.
        "process_tree",
        metal::Metal { arms: PROCTREECASE, judge: |b| process_tree(b[0]) },
    ),
    (
        // A launched row runs its own program, never what the caller's path
        // names when it is opened again.
        "launch_toctou",
        metal::Metal { arms: PROCTREECASE, judge: |b| b[0].job_passed("test_rs_launch_toctou") },
    ),
    (
        // A launch starts only what the caller's row lists, and `swap` and
        // `update` only in a login session.
        "launch_authority",
        metal::Metal { arms: PROCTREECASE, judge: |b| launch_authority(b[0]) },
    ),
    (
        // A connection's badge is the minted bytes, read back by its own
        // port's acceptor alone.
        "port_badge",
        metal::Metal { arms: PROCTREECASE, judge: |b| b[0].job_passed("test_rs_port_badge") },
    ),
    (
        // One share holding all a file server lets it hold leaves the server
        // answering a login session, whose launches all spend its one share.
        "fs_share",
        metal::Metal { arms: PROCTREECASE, judge: |b| b[0].job_passed("test_rs_fs_share") },
    ),
    // ---- one image: tests/metalcase, which runs no job ----
    //
    // The rows after the first read what any boot that hands the machine back
    // writes, so they ride this one.
    (
        "metal_sim_scanout_wc",
        metal::Metal { arms: METALCASE, judge: |b| scanout_wc(b[0].kernel().text()) },
    ),
    (
        // Two boots the suite already flashes: the device boot writes and
        // fsyncs megabytes before its reset, and `metalcase` is the same reset
        // with no job behind it.
        "usb_reset_hands_devices_back",
        metal::Metal {
            arms: USB_RESET_BOOTS,
            judge: power::usb_reset_on_metal,
        },
    ),
    (
        // On the T14 the chain is what every metal boot does — the loader
        // points `BootNext` at itself before each handoff, so the pass that
        // reads the page appends its report to the same `loader.log` the
        // driver hands back.
        "blackbox_done_chain",
        metal::Metal { arms: METALCASE, judge: |b| power::done_chain(&b[0].after_the_reset()?) },
    ),
    (
        // The machine came back to Ubuntu's ssh server, which is what tells a reset from the
        // S5 power-off the QEMU stop reason exists to catch — the driver
        // established it before this judge ran. What is left is the kernel's own
        // decode, and `0xcf9 <- 0x0f` is q35's register rather than this one's.
        "machine_reboot",
        metal::Metal {
            arms: METALCASE,
            judge: |b| {
                power::reset_register_decoded(&b[0].kernel())?;
                bootlog::handed_back(b[0].after_the_reset()?.text()).map_err(|why| why.to_string())
            },
        },
    ),
    // ---- the `SYS_DEBUG` members' boot, armed ----
    // The cheapest cluster there is: every one of these arms a check that runs
    // at init, logs its verdict and does nothing else, so they cost no flash
    // of their own. **Nothing had to be promoted into `kernel/src/params.rs`**
    // — the metal profile flashes test images (the track's ruling), and the
    // pre-flash gate is what says the machine survives each one.
    (
        "pci_capability_walk",
        metal::Metal { arms: SELFTESTS, judge: |b| pci_cap_selftest(b[0].kernel().text()) },
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
    (
        // A unit handed over translating, remapping and queueing, on real
        // silicon: each field goes off by its own write before the unit is
        // programmed, and the boot goes on.
        "iommu_firmware_left",
        metal::Metal { arms: SELFTESTS, judge: |b| iommu_firmware_left(b[0].kernel().text()) },
    ),
    // ---- a claimed function at the unit, on tests/testcases ----
    (
        // Claimed, given back and claimed again: both claims name one
        // remapping entry, and each release leaves it not present.
        "claim_reuses_its_remapping_entry",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed(claims::RECLAIM)?;
                claims::reuses_its_entry(&b[0].kernel())
            },
        },
    ),
    (
        // Every domain's addresses end below the first root-bridge window
        // above where they start.
        "domain_ends_below_the_host_bridges",
        metal::Metal { arms: TESTCASES, judge: |b| claims::clear_of_host_bridges(&b[0].kernel()) },
    ),
    (
        // A machine whose units do not remap: the claim is refused before
        // anything on the function changes, and the boot goes on.
        "claim_refused_without_remapping",
        metal::Metal {
            arms: &[metal::once(
                "iommu-no-remap",
                "tests/testcases",
                &["iommu-no-remap"],
                &[claims::RECLAIM],
            )],
            judge: |b| claims::refused_unremapped(&b[0].kernel()),
        },
    ),
    // ---- the `isa` claim: one image whose i8042 the kernel leaves alone ----
    (
        // The I/O permission bitmap on the machine's own processor: the ports
        // open to the process that bound them, and every other access killed
        // by name.
        "isa_ports_are_the_binders_alone",
        metal::Metal {
            arms: ISA_WITHHELD,
            judge: |b| {
                b[0].job_passed("test_rs_isa_grant")?;
                isa::ports(&b[0].kernel())
            },
        },
    ),
    (
        // The machine's own controller raises its line through the I/O APIC to
        // the claim's holder, and to nobody once the claim is gone.
        "isa_lines_reach_their_holder",
        metal::Metal {
            arms: ISA_WITHHELD,
            judge: |b| {
                b[0].job_passed("test_rs_isa_lines")?;
                isa::lines(&b[0].kernel())
            },
        },
    ),
    (
        "crash_report_reads_no_kernel_memory",
        metal::Metal {
            arms: &[metal::once("testcases", "tests/testcases", &[], &["test_rs_fault_gates"])],
            judge: |b| {
                b[0].job_passed("test_rs_fault_gates")?;
                faults::crash_report_reads_no_kernel_memory(&b[0].kernel())
            },
        },
    ),
];

/// A boot whose kernel leaves the i8042 unprobed, so the one grantable row is
/// free: the ports' job first, since its last holder keeps them until it ends.
const ISA_WITHHELD: &[metal::Arm] = &[metal::once(
    "isa-withheld",
    "tests/testcases",
    &["i8042-withheld"],
    &["test_rs_isa_grant", "test_rs_isa_lines"],
)];

/// The boot most of the first tranche rides: the plain `tests/testcases` shape
/// with a job list that ends it.
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
        "test_rs_syscall_cost",
        "test_rs_null_sink_client_exits",
        claims::RECLAIM,
    ],
)];

/// The same boot, ended on the hold: the count it waits for comes thirty
/// seconds after the server arms, and a job behind it would wait that out too.
const TESTCASES_HELD: &[metal::Arm] = &[metal::Arm {
    last: Some("test_rs_acpi_hold"),
    ..metal::once("testcases", "tests/testcases", &[], &[])
}];

/// **Two boots of one config, because these two cannot share one.** Each fills
/// a machine-wide cap and leaves it filled: `mkdir_cap` fills the directory cap,
/// and `readdir_bound`'s own `create_dir("/tmp/empty")` is then refused with
/// `OutOfMemory` and it panics — measured on the first staged image.
const TESTCASES_MKDIR: &[metal::Arm] =
    &[metal::once("testcases-mkdir", "tests/testcases", &[], &["test_rs_mkdir_cap"])];

const TESTCASES_READDIR: &[metal::Arm] =
    &[metal::once("testcases-readdir", "tests/testcases", &[], &["test_rs_readdir_bound"])];

/// One boot for the three, which can share it: each fills a bound of its own
/// process, and its exit gives all of it back.
const BOUNDS: &[metal::Arm] = &[metal::once(
    "testcases-bounds",
    "tests/testcases",
    &[],
    &["test_rs_abuse_mmap_regions", "test_rs_abuse_thread_table", "test_rs_abuse_dlopen_ledger"],
)];

/// The shipping kernel with the windows' instrument and nothing else, so what
/// it reads is that kernel under the herd. Three exits, a report each:
/// `idle_span`'s is the boot's first, where the kernel holds; `pwd`'s reads the
/// hold back and empties every record; the herd's is the herd's reading.
const WINDOWSCASE: &[metal::Arm] = &[metal::Arm {
    features: toyos_build::build::MASK_WINDOWS_KERNEL,
    ..metal::once(
        "windowscase",
        "tests/testcases",
        &[],
        &["test_rs_idle_span", "pwd", WINDOWS_LOAD],
    )
}];

/// The load `mask_windows` reads the windows under.
const WINDOWS_LOAD: &str = "test_rs_ring_park_herd";

/// The head of the kernel's record of [`WINDOWS_LOAD`]'s exit.
fn windows_load_exited() -> String {
    format!("{}{} pid=", toyos_build::bootlog::EXIT, toyos_build::bootlog::recorded_name(WINDOWS_LOAD))
}

/// A `logkeeper` that leaves soundserver's ring unread until the job says the tone played.
const LOGSTALLCASE: &[metal::Arm] =
    &[metal::once("logstallcase", "tests/logstallcase", &[], &["test_rs_soundserver_log_stall"])];

/// The two boots the reset ruling is judged on: the device boot for a reset
/// with megabytes behind it, and `metalcase` for one with no job behind it.
const USB_RESET_BOOTS: &[metal::Arm] = &[
    metal::once(
        devices::BOOT,
        devices::CONFIG,
        &[],
        devices::JOBS,
    ),
    metal::once("metalcase", "tests/metalcase", &[], &[]),
];

const METALCASE: &[metal::Arm] = &[metal::once("metalcase", "tests/metalcase", &[], &[])];

/// A launcher and a declared `cat` and shell, which `process_tree`'s subtree
/// launches, a `toybox` row holding `roster`, which `launch_toctou` races, the
/// rows `launch_authority` is refused and started, and the shells `fs_share`
/// asks DATA's server through, under its share and in a login session.
const PROCTREECASE: &[metal::Arm] = &[metal::once(
    "proctreecase",
    "tests/proctreecase",
    &[],
    &[
        "test_rs_process_tree",
        "test_rs_launch_toctou",
        "test_rs_launch_authority",
        "test_rs_port_badge",
        "test_rs_fs_share",
    ],
)];

/// Every in-kernel self-test that logs its verdict at init and does nothing
/// else.
///
/// They cost the machine no flash of their own because none of them changes
/// what the machine *is*: each stages inputs the hardware cannot produce — a
/// crafted capability list, a malformed descriptor, a vector nothing claims —
/// runs a check over them and prints a count. So they arm the boot
/// [`ACTUATOR_TESTS`] ride, which is the same kernel.
const SELFTEST_PARAMS: &[&str] = &[
    "pci-cap-selftest",
    "revoked-backing-selftest",
    "leak-rollback-selftest",
    "lapic-spurious-selftest",
    "unclaimed-vector-selftest",
    "xhci-xecp-selftest",
    "xhci-descriptor-selftest",
    "sysret-ss-probe",
    "test-input-merge",
    "sched-operation-nesting",
    "iommu-firmware-left",
];

const SELFTESTS: &[metal::Arm] = &[metal::Arm {
    features: ACTUATOR_KERNEL,
    ..metal::once("shared-debug", "tests/testcases", SELFTEST_PARAMS, &[])
}];

/// The boots every discovered Rust binary rides on the T14: the shipping
/// kernel's, and [`ACTUATOR_TESTS`] on the kernel that carries `SYS_DEBUG`,
/// armed with [`SELFTEST_PARAMS`].
fn shared_metal() -> Vec<metal::SharedBoot> {
    let (debug, shipping): (Vec<String>, Vec<String>) = discover_rust_tests()
        .into_iter()
        .partition(|name| ACTUATOR_TESTS.contains(&name.as_str()));
    vec![
        metal::SharedBoot {
            boot: "shared".to_string(),
            config: "tests/testcases",
            params: &[],
            features: &[],
            members: const { metal::members_fitting(toyos_tco::RUST_MEMBER_MS) },
            jobs: shipping.iter().map(|n| format!("test_rs_{n}")).collect(),
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
            params: SELFTEST_PARAMS,
            features: ACTUATOR_KERNEL,
            members: const { std::num::NonZeroUsize::new(18).expect("a chunk holds a member") },
            jobs: debug.iter().map(|n| format!("test_rs_{n}")).collect(),
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
fn c_corpus_metal(c_bins: &[(String, Vec<u8>)]) -> metal::SharedBoot {
    let mut jobs = Vec::new();
    let mut files = Vec::new();
    let mut links = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for (case, data) in c_bins {
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
        members: const { metal::members_fitting(toyos_tco::C_MEMBER_MS) },
        jobs,
        files,
        links,
    }
}

/// The comparator's own staged name. It is a `RUST_SKIP` helper, so discovery
/// never makes a job of it and [`shared_metal`] stages it as one.
const CCHECK: &str = "test_rs_ccheck";
/// The renderer's inks for `Info` text, `Alert` text and a record's head, as
/// the screendump reports them.
const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];
const ALERT: [u8; 3] = [0xFF, 0x6E, 0x6E];
const STAMP: [u8; 3] = [0x9E, 0x9E, 0x9E];
/// And the fill a halted machine leaves behind.
const FILL_FATAL: [u8; 3] = [0x60, 0x00, 0x00];
/// The fill a boot checkpoint leaves behind. It is the only thing that tells a
/// diagnostic boot's screen from a fatal report's — both carry the same log
/// lines, and one of them means the machine died.
const FILL_BOOT: [u8; 3] = [0x00, 0x00, 0x00];

/// The line `SYS_DEBUG` action 3 logs immediately before halting every CPU.
/// It exists only on a `test-actuators` kernel — every other action costs the
/// caller its own process, this one costs the machine.
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
        why: Why::Declined("it prints `long double`s through `%Lf`, and libc reads a `long double` as a `double` (issues/libc-reads-a-long-double-as-a-double.md): every `%Lf` of a line whose `double`s filled the registers prints 0.000000"),
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
        stage: Stage::Refused("'semaphore.h' file not found"),
        why: Why::Declined("semaphores, sigjmp_buf and a signal's handler run, none of which libc has"),
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

/// Assert the colour decisions `text()` cannot see: the fill, the text of
/// every row an `alert!` produced and of every row carrying a text it did not,
/// and the head the record's first row opens with.
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
    for alert_line in alert_lines {
        inked(
            dump,
            alert_line,
            ALERT,
            "every row of an `alert!` record wears its level, including the ones its message \
             wrapped or newlined onto",
        )?;
    }
    // The record's first row opens with its head, drawn dim and apart from its text.
    let opens = alert_lines.first().and_then(|line| dump.row_index(line));
    if let Some(cy) = opens.filter(|&cy| dump.row_fg(cy) != Some(STAMP)) {
        return Err(format!(
            "the head of {:?} drawn in {:?}, want the stamp's {STAMP:?}\n{}",
            dump.rows()[cy],
            dump.row_fg(cy),
            dump.text()
        ));
    }
    inked(
        dump,
        plain_line,
        WHITE,
        "an ordinary record's text is white on every row it fills, a row its first line \
         wrapped onto included, which carries no head",
    )
}

/// `needle`'s own cells are drawn in `want` on every row carrying it, and
/// some row does.
fn inked(dump: &screen::Ppm, needle: &str, want: [u8; 3], why: &str) -> Result<(), String> {
    let rows = dump.fg_of(needle);
    if rows.is_empty() {
        return Err(format!("{needle:?} not on screen\n{}", dump.text()));
    }
    match rows.iter().find(|(_, fg)| *fg != Some(want)) {
        Some((row, fg)) => {
            Err(format!("{needle:?} drawn in {fg:?} on {row:?}, want {want:?} — {why}\n{}", dump.text()))
        }
        None => Ok(()),
    }
}

/// `tests/toyos-rust-tests`' binary that `tests/virtjobcase` runs as its job
/// `test_rs_abuse_readonly_copyout`.
const VIRT_COPYOUT: &str = "abuse_readonly_copyout";

/// The same for its job `test_rs_ring0_timer_in_syscall`.
const VIRT_RING0_TIMER: &str = "ring0_timer_in_syscall";

/// `tests/toyos-rust-tests`' binary that `tests/virtsmpcase` runs as its job
/// `test_rs_counters_read`.
const VIRT_COUNTERS_READ: &str = "counters_read";

/// `tests/toyos-rust-tests`' binary that `tests/virtsmpcase` runs as its job
/// `test_rs_trace_read`.
const VIRT_TRACE_READ: &str = "trace_read";

/// `tests/toyos-rust-tests`' binary `name` built for `arch`, once a run, as a
/// file a case's job list names on ROOT.
fn suite_bin(arch: toyos_build::arch::Arch, name: &'static str) -> (String, Vec<u8>) {
    type Built = Vec<(toyos_build::arch::Arch, &'static str, Vec<u8>)>;
    static BUILT: std::sync::Mutex<Built> = std::sync::Mutex::new(Vec::new());
    let mut built = BUILT.lock().expect("no build panics holding this");
    let bytes = match built.iter().find(|(a, n, _)| *a == arch && *n == name) {
        Some((.., bytes)) => bytes.clone(),
        None => {
            let bytes = qemu::build_toyos_bin(arch, &compile::repo_root().join("tests/toyos-rust-tests"), name);
            built.push((arch, name, bytes.clone()));
            bytes
        }
    };
    (format!("bin/test_rs_{name}"), bytes)
}

/// Boot `tests/virtjobcase` on one CPU and judge its job `job`: it ends with
/// exit 0, having said `said`. One CPU because `preempt` and `fp_isolation`
/// see a sibling run only when it took theirs. The kernel carries `SYS_DEBUG`
/// for `debug_refused`, and every job runs in every boot of the case.
fn virt_job(profile: qemu::Profile, job: &str, said: &str) -> Result<(), String> {
    let config = compile::repo_root().join("tests/virtjobcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    let mut qemu = QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            profile,
            smp: 1,
            kernel_features: toyos_build::build::TEST_KERNEL,
            ready_marker: "control registers: SCTLR_EL1=",
            extra_root_files: vec![suite_bin(profile.arch(), VIRT_COPYOUT), suite_bin(profile.arch(), VIRT_RING0_TIMER)],
            ..Default::default()
        },
    );
    let mut serial = virt_console(&qemu);
    judge_virt_job(&mut qemu, &mut serial, job, said)?;
    // The kernel's record `one_clock` reads beside the supervisor's line: it
    // reaches the PL011 by the kernel's road, and the job's end by `logkeeper`'s.
    await_marker(&mut qemu, &mut serial, bootlog::LOGKEEPER_SPAWN, "the kernel's record of logkeeper's spawn")?;
    // The one console that carries the loader's, the kernel's and a program's
    // lines on a CPU that states its counter's rate: the T14's metal rows
    // judge the same on x86-64, and no machine of this architecture has one.
    bootlog::one_clock(&serial, &serial).map_err(|why| format!("{why}\nserial:\n{serial}"))
}

/// What `acpi_mediated` says, an arm a line, once the kernel answered each as
/// its policy says.
const ACPI_MEDIATED_SAID: [&str; 10] = [
    "acpi: an unbound claim was refused its access, the lock and the power-off's sleep type",
    "acpi: RAM was refused both ways as UsableMemory",
    "acpi: the RSDP read through as type 9 and its write was refused TableWrite",
    "acpi: an unlisted register was read and refused its write MemoryType, an unlisted address below 1 MiB was refused UnlistedCached, and the interrupt controllers, the HPET and a function's BAR DeviceMemory",
    "acpi: the FACS read through as type 10, its write was refused FacsWrite, and the memory after it was written and put back",
    "acpi: COM1, the CMOS index, the 8259 and the configuration mechanism were refused KernelPort; the i8042's row ClaimedPort; PM1a_CNT read and refused its write ReadOnlyPort; the POST port was written",
    "acpi: ACPI_ENABLE and ACPI_DISABLE were refused SMI_CMD as KernelCommand and SCI_EN stayed set, a write wider than a byte was refused CommandSpan, every firmware call made was read back from the port and counted on the boot processor, and a storm of them was refused CommandRate",
    "by its address and through ECAM, and every write to configuration space was refused ConfigWrite",
    "acpi: the lock a dead holder left taken read free; it was taken and given back, given back with GBL_RLS where the firmware had asked, and found pending while the firmware owned it",
    "acpi: a sleep type wider than three bits was refused InvalidArgument, the next holder's replaced a dead one's, and a second under one claim was refused AlreadyExists",
];

/// Boot `tests/acpicase`, whose one job is `test_rs_acpi_mediated`, on the
/// test kernel, and judge the job and what the kernel said beside it: the
/// lock found at boot, given back for the holder that died with it, and
/// given back for the probe itself, which asks for the power-off holding it
/// once every arm has passed; and the power-off, which this boot's kernel
/// refuses by name and stops nothing for while no holder has supplied its
/// sleep type, keeps one sleep type of under each claim, and makes on the
/// probe's and not on the dead holder's before it, which QEMU's ICH9 answers
/// by stopping the guest only for the probe's. The probe does not come back
/// from that, so its verdict is its last line, the kernel's and QEMU's; a
/// probe that ends instead is one whose arm failed. On `cpus` CPUs: the
/// second one's range registers are read where there is one.
///
/// And the probe's calls into the firmware, the first write to `SMI_CMD`
/// this kernel makes on a guest, whose firmware hands it over in ACPI mode:
/// the kernel says the first call of each byte, five of them, each written
/// on the boot processor by that CPU's own reading beside the `out`, and
/// where there is a second CPU one at least asked from it, the probe having
/// asked three from a thread that read itself there.
fn acpi_mediated_access(cpus: u32) -> Result<(), String> {
    const JOB: &str = "test_rs_acpi_mediated";
    const HELD_INTO_THE_STOP: &str = "acpi: holding the Global Lock, and asking for the power-off with it";
    const GIVEN_BACK_AT_THE_STOP: &str = "acpi: the Global Lock given back for a holder that left it taken (the machine is stopping)";
    const NO_S5: &str = "shutdown: no ACPI server supplied S5 — refused";
    let case = compile::repo_root().join("tests/acpicase");
    let mut qemu = QemuInstance::boot_with_options(
        &case,
        &[],
        &[],
        BootOptions {
            // The test kernel, for the Global Lock's actuator too.
            kernel_params: &["i8042-withheld"],
            ready_marker: "acpi: the ACPI row: ",
            smp: cpus,
            extra_root_files: vec![suite_bin(toyos_build::arch::Arch::X86_64, "acpi_mediated")],
            qmp: true,
            ..Default::default()
        },
    );
    // Opened before the probe asks: QMP delivers no event emitted before its
    // client connected.
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    let mut console = format!("{}\n", qemu.boot_log());
    let ended = format!("===TEST_END {JOB} ");
    await_guest(&mut qemu, &mut console, "the probe's power-off to give the lock back", |said| {
        said.contains(GIVEN_BACK_AT_THE_STOP) || said.contains(&ended)
    })?;
    let said = serial::Serial::named("the probe's boot", console);
    said.must_be_clean()?;
    said.must_say(isa::WITHHELD)?;
    said.must_say("acpi: the Global Lock is the FACS's at ")?;
    said.must_say("acpi: the Global Lock given back for a holder that left it taken (its claim is gone)")?;
    // Whose range registers passed the unlisted read, and how this
    // hypervisor's second CPU holds its own beside them.
    let mtrr = ["mtrr: the boot processor's range registers: ", "mtrr: cpu1's range registers are "];
    for line in mtrr.iter().take(cpus as usize) {
        eprintln!("  [acpi] {}", said.must_say(line)?.trim());
    }
    for line in ACPI_MEDIATED_SAID {
        eprintln!("  [acpi] {}", said.must_say(line)?.trim());
    }
    for reading in ["acpi: firmware calls of ", " firmware calls were made before call "] {
        eprintln!("  [acpi] {}", said.must_say(reading)?.trim());
    }
    let calls: Vec<&str> = said.text().lines().filter(|l| l.contains("acpi: firmware call 0x") && l.contains(" written to SMI_CMD ")).collect();
    if calls.len() != 5 {
        return Err(format!("the kernel said {} first firmware calls where the probe made calls of five bytes: {calls:#?}", calls.len()));
    }
    let mut crossed = 0;
    for line in &calls {
        smi_cmd_writer(line)?;
        crossed += usize::from(number_between(line, ", asked from cpu", "; the write held cpu")? != 0);
        eprintln!("  [acpi] {}", line.trim());
    }
    if cpus > 1 && crossed == 0 {
        return Err(format!("no firmware call was asked from another CPU than the boot processor, so none crossed to it: {calls:#?}"));
    }
    // The shutdown refused by the kernel's own name for it, before anything
    // was stopped: the probe ran on and said every line above.
    eprintln!("  [acpi] {}", said.must_say(NO_S5)?.trim());
    // One line a claim, the dead holder's and then the probe's: no refused
    // word was kept, and no second one under either claim.
    let supplied = s5_supplied(&said);
    if supplied != ["0x604 with SLP_TYPa=5, as the acpi claim's holder supplied it", "0x604 with SLP_TYPa=0, as the acpi claim's holder supplied it"] {
        return Err(format!("the kernel kept {supplied:#?}, where the keeper supplied 5 and the probe 0, once each:\n{}", said.text()));
    }
    said.must_say_after(NO_S5, S5_SUPPLIED)?;
    said.must_say(HELD_INTO_THE_STOP)?;
    eprintln!("  [acpi] {}", said.must_say_after(HELD_INTO_THE_STOP, GIVEN_BACK_AT_THE_STOP)?.trim());
    let stopped = stop.reason();
    if stopped.as_deref() != Some("guest-shutdown") {
        return Err(format!("QEMU stopped this guest for {stopped:?}, not for the power-off on the probe's sleep type:\n{}", said.text()));
    }
    eprintln!("  [acpi] QEMU stopped the guest for guest-shutdown, on the probe's sleep type and not the keeper's");
    Ok(())
}

/// The head of the kernel's line for a sleep type it kept.
const S5_SUPPLIED: &str = "power: S5 is PM1a ";

/// What the kernel said after [`S5_SUPPLIED`], a line a sleep type it kept.
fn s5_supplied(said: &serial::Serial) -> Vec<&str> {
    said.text().lines().filter_map(|line| Some(line.split_once(S5_SUPPLIED)?.1.trim())).collect()
}

/// Boot `tests/acpicase` on the shipping kernel with `acpi_mediated`'s
/// `outlived` arm staged: a holder supplies q35's own sleep type and exits,
/// its claim and one more that supplied nothing are released, and the probe,
/// holding no claim, asks for the power-off. The kernel kept one sleep type,
/// and QEMU's ICH9 stops the guest on it: what a holder supplied stands once
/// the holder is gone. A probe that ends instead was refused its power-off.
fn acpi_supply_outlives_holder() -> Result<(), String> {
    const JOB: &str = "test_rs_acpi_mediated";
    const ASKED: &str = "acpi: asking for the power-off with no claim held, on what a holder that is gone supplied";
    let case = compile::repo_root().join("tests/acpicase");
    let mut qemu = QemuInstance::boot_with_options(
        &case,
        &[],
        &[],
        BootOptions {
            ready_marker: "acpi: the ACPI row: ",
            extra_root_files: vec![
                suite_bin(toyos_build::arch::Arch::X86_64, "acpi_mediated"),
                // The arm's name is the file's: `acpi_mediated` asks whether it is there.
                ("share/acpi_mediated_outlived".to_string(), b"outlived\n".to_vec()),
            ],
            qmp: true,
            ..Default::default()
        },
    );
    // Opened before the probe asks: QMP delivers no event emitted before its
    // client connected.
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    let mut console = format!("{}\n", qemu.boot_log());
    let ended = format!("===TEST_END {JOB} ");
    await_guest(&mut qemu, &mut console, "the power-off on a sleep type its holder left behind", |said| {
        said.contains(power::SHUTTING_DOWN) || said.contains(&ended)
    })?;
    let said = serial::Serial::named("the supplier's boot", console);
    said.must_be_clean()?;
    let supplied = s5_supplied(&said);
    if supplied != ["0x604 with SLP_TYPa=0, as the acpi claim's holder supplied it"] {
        return Err(format!("the kernel kept {supplied:#?}, where one holder supplied 0, once:\n{}", said.text()));
    }
    said.must_say(ASKED)?;
    eprintln!("  [acpi] {}", said.must_say_after(ASKED, power::SHUTTING_DOWN)?.trim());
    let stopped = stop.reason();
    if stopped.as_deref() != Some("guest-shutdown") {
        return Err(format!("QEMU stopped this guest for {stopped:?}, not for the power-off on the sleep type a dead holder supplied:\n{}", said.text()));
    }
    eprintln!("  [acpi] QEMU stopped the guest for guest-shutdown, with no claim held, on the sleep type a holder that is gone supplied");
    Ok(())
}

/// Boot the image `--console-boot` builds, on the machine shape with a panel
/// for `/system/bin/console` to claim, and wait for what that image owes at
/// boot: the console up on its panel, having read the log and the keyboard.
/// Nothing is typed at its shell, whose only input is the i8042:
/// `issues/the-console-loses-typed-keystrokes-under-host-load.md`.
fn console_image_boots() -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        &compile::repo_root().join("console"),
        &[],
        &[],
        BootOptions { profile: qemu::Profile::Metal, ready_marker: bootlog::COMPLETE, ..Default::default() },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_marker(&mut qemu, &mut console, "console: ready ", "the console to take its panel")?;
    serial::Serial::named("the console image's boot", console).must_be_clean()
}

/// A `mask-windows` boot's windows: `common::irqcensus::windows`'s verdict,
/// with `cpus` CPUs reporting.
fn mask_windows(capture: &str, cpus: u32) -> Result<(), String> {
    let longest = common::irqcensus::windows(capture)?;
    if longest.len() != cpus as usize {
        return Err(format!("{} of {cpus} CPUs reported windows: {longest:?}", longest.len()));
    }
    for most in longest.values() {
        eprintln!(
            "  [windows] cpu{} irqs_off_ns={} preempt_off_ns={}",
            most.cpu, most.irqs_off_ns, most.preempt_off_ns
        );
    }
    Ok(())
}

/// Everything the PL011 has carried on `qemu`'s boot: the capture each
/// [`judge_virt_job`] after the first goes on from, since a drain reads past
/// the marker it waited for.
fn virt_console(qemu: &QemuInstance) -> String {
    format!("{}\n", qemu.boot_log())
}

/// Wait for `job`'s end on a guest booted with it, and judge it: it ends with
/// exit 0, having said `said`. `serial` is everything the PL011 has carried,
/// and takes what this drains.
fn judge_virt_job(qemu: &mut QemuInstance, serial: &mut String, job: &str, said: &str) -> Result<(), String> {
    let end = format!("===TEST_END {job} ");
    if let Err(why) = await_marker(qemu, serial, &end, &format!("the job {job} to end")) {
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
    Ok(())
}

/// What `unmap_touch` says once every read of a page just unmapped,
/// on the unmapping thread and on another, ended its process.
const UNMAP_TOUCH_SAID: &str =
    "unmap_touch: 4 reads of a page just unmapped on the unmapping thread, and 4 on another";

/// The CPUs `virt_smp` boots.
const VIRT_CPUS: u32 = 8;

/// The windows on AArch64: `tests/virtsmpcase` on [`VIRT_CPUS`] CPUs of a
/// `mask-windows` kernel, whose every hook checks the state it finds, judged
/// on the whole console once the case's job `unmap_touch` has ended and the
/// boot has said its last word.
fn virt_mask_windows(profile: qemu::Profile) -> Result<(), String> {
    let mut qemu = boot_virt_smp(BootOptions {
        profile,
        smp: VIRT_CPUS,
        kernel_features: toyos_build::build::MASK_WINDOWS_KERNEL,
        ..Default::default()
    });
    let mut serial = virt_console(&qemu);
    judge_virt_job(&mut qemu, &mut serial, "unmap_touch", UNMAP_TOUCH_SAID)?;
    // To the boot's last word, said after every report: the drain that took the job's end can stop inside one.
    await_marker(&mut qemu, &mut serial, power::SHUTTING_DOWN, "the boot's last word")?;
    mask_windows(&serial, VIRT_CPUS)
}

/// Boot `tests/virtsmpcase` as `options` say.
fn boot_virt_smp(options: BootOptions) -> QemuInstance {
    let config = compile::repo_root().join("tests/virtsmpcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            ready_marker: "control registers: SCTLR_EL1=",
            extra_root_files: vec![
                suite_bin(options.profile.arch(), VIRT_COUNTERS_READ),
                suite_bin(options.profile.arch(), VIRT_TRACE_READ),
            ],
            ..options
        },
    )
}

/// What `counters_read` says once every CPU answered for itself.
const COUNTERS_READ_SAID: &str = "counters_read: every cpu answered for itself, and each counter is a right's";

/// What `trace_read` says once the diary read back as the scheduler wrote it.
const TRACE_READ_SAID: &str = "trace_read: a wake precedes its pick, a cursor is its reader's own, and the diary is TRACE's";

/// Boot `tests/virtsmpcase` on [`VIRT_CPUS`] CPUs under `profile`, whose
/// firmware enters every CPU at EL`el` and whose FADT names PSCI's `conduit`:
/// each CPU is started by `CPU_ON`, holds the control-register declaration as
/// entered there and joins the scheduler, and the case's jobs `unmap_touch`,
/// `test_rs_counters_read` and `test_rs_trace_read` end with exit 0. Then its job `shutdown` stops the machine and powers it
/// off, every other CPU turned off first ([`psci_powered_off`]).
fn virt_smp(profile: qemu::Profile, conduit: &str, el: u32) -> Result<(), String> {
    let trace = common::lane::dir().join(format!("virt_smp-{conduit}.psci"));
    let mut qemu = boot_virt_smp(BootOptions {
        profile,
        smp: VIRT_CPUS,
        qmp: true,
        psci_trace: Some(trace.clone()),
        ..Default::default()
    });
    // Before the job list can reach its `shutdown`: QMP delivers no event
    // emitted before its client connected.
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    let mut serial = virt_console(&qemu);
    judge_virt_job(&mut qemu, &mut serial, "unmap_touch", UNMAP_TOUCH_SAID)?;
    let psci = serial.lines().find(|l| l.contains("PSCI: ")).unwrap_or_default();
    if !psci.contains(&format!(" through {conduit}")) {
        return Err(format!("PSCI is not said to be reached through {conduit}: {psci:?}\nserial:\n{serial}"));
    }
    let mut want = vec![format!("SMP: {VIRT_CPUS} of {VIRT_CPUS} MADT CPUs online")];
    for cpu in 1..VIRT_CPUS {
        want.push(format!("SMP: cpu{cpu} mpidr={cpu:#x} online"));
    }
    for want in want {
        if !serial.contains(&want) {
            return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
        }
    }
    // Waited for: a CPU says this once the machine is released, so it reaches
    // the PL011 by the kernel's road beside the job's end on `logkeeper`'s.
    for cpu in 1..VIRT_CPUS {
        let joined = format!("CPU {cpu}: joining scheduler");
        await_marker(&mut qemu, &mut serial, &joined, &format!("cpu{cpu} to join the scheduler"))?;
    }
    let entered = format!("as declared; entered at EL{el}");
    for cpu in 0..VIRT_CPUS {
        if !serial.lines().any(|l| record_cpu(l) == Some(cpu) && l.contains(&entered)) {
            return Err(format!("cpu{cpu} never said its registers are {entered:?}\nserial:\n{serial}"));
        }
    }
    eprintln!("  [virt] {VIRT_CPUS} CPUs entered at EL{el}, started through {conduit}, and scheduling");
    judge_virt_job(&mut qemu, &mut serial, "test_rs_counters_read", COUNTERS_READ_SAID)?;
    judge_virt_job(&mut qemu, &mut serial, "test_rs_trace_read", TRACE_READ_SAID)?;
    let (console, calls) =
        ended_through_psci(&mut qemu, &mut stop, serial, power::SHUTTING_DOWN, "guest-shutdown", &trace, |_| Vec::new())?;
    let record = console
        .lines()
        .find_map(toyos_quiesce::Record::parse)
        .ok_or_else(|| format!("the kernel wrote no stop record\n{console}"))?;
    if record.cpus != VIRT_CPUS {
        return Err(format!("the stop ran across {} CPUs, not {VIRT_CPUS}: {record}\n{console}", record.cpus));
    }
    let last = psci_powered_off(&calls, VIRT_CPUS, &[])?;
    eprintln!("  [virt] {record}; every CPU but {last:#x} called CPU_OFF, then {last:#x} SYSTEM_OFF");
    Ok(())
}

/// PSCI function IDs, as `arm_psci_call` prints `x0` (Arm DEN0022 §5.1).
const PSCI_CPU_OFF: u64 = 0x8400_0002;
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

/// [`power::ended`] on a guest whose job list asked for its end; `console` is
/// what it has said so far. What follows the last word is `said_after` of the
/// calls, line for line, and nothing else. Answers the whole console and every
/// PSCI call QEMU traced into `trace`, in order.
fn ended_through_psci(
    qemu: &mut QemuInstance,
    stop: &mut qemu::QmpShutdown,
    mut console: String,
    last: &str,
    reason: &str,
    trace: &Path,
    said_after: impl FnOnce(&[(u64, u64)]) -> Vec<String>,
) -> Result<(String, Vec<(u64, u64)>), String> {
    power::ended(qemu, stop, &mut console, last, reason)?;
    let lines: Vec<&str> = console.lines().collect();
    let at = lines.iter().position(|l| l.contains(last)).expect("awaited above");
    let after: Vec<&str> = lines[at + 1..].iter().copied().filter(|l| !l.trim().is_empty()).collect();
    let traced = fs::read_to_string(trace).map_err(|e| format!("read the PSCI trace: {e}"))?;
    let calls = psci_calls(&traced)?;
    let want = said_after(&calls);
    if !after.iter().map(|l| l.trim_end()).eq(want.iter().map(String::as_str)) {
        return Err(format!(
            "{} line(s) after the boot's last word, not {want:?}:\n  {}",
            after.len(),
            after.join("\n  ")
        ));
    }
    Ok((console, calls))
}

/// Every call QEMU's `arm_psci_call` trace `text` records, in order: the
/// function (`x0`) and the affinity of the CPU that made it (`cpuid`).
fn psci_calls(text: &str) -> Result<Vec<(u64, u64)>, String> {
    let hex = |line: &str, key: &str| -> Option<u64> {
        let digits: String =
            line.split(key).nth(1)?.strip_prefix("0x")?.chars().take_while(char::is_ascii_hexdigit).collect();
        u64::from_str_radix(&digits, 16).ok()
    };
    text.lines()
        .filter(|l| l.contains("arm_psci_call"))
        .map(|l| match (hex(l, " x0="), hex(l, " cpuid=")) {
            (Some(function), Some(cpu)) => Ok((function, cpu)),
            _ => Err(format!("a PSCI trace line this reader does not know: {l:?}")),
        })
        .collect()
}

/// A power-off by PSCI's own recipe (DEN0022 §5.10.3), read off QEMU's trace
/// of a machine of `cpus` CPUs whose affinities are `0..cpus`: every CPU but
/// one and those `left_on` called `CPU_OFF`, each once, and then the one left
/// called `SYSTEM_OFF`, once. Answers that CPU.
fn psci_powered_off(calls: &[(u64, u64)], cpus: u32, left_on: &[u64]) -> Result<u64, String> {
    let offs: Vec<usize> = (0..calls.len()).filter(|&i| calls[i].0 == PSCI_SYSTEM_OFF).collect();
    let [at] = offs[..] else {
        return Err(format!("QEMU traced {} SYSTEM_OFF calls, not one: {calls:x?}", offs.len()));
    };
    if calls.iter().any(|&(function, _)| function == PSCI_SYSTEM_RESET) {
        return Err(format!("a SYSTEM_RESET beside the power-off: {calls:x?}"));
    }
    let last = calls[at].1;
    let mut turned_off: Vec<u64> =
        calls[..at].iter().filter(|&&(function, _)| function == PSCI_CPU_OFF).map(|&(_, cpu)| cpu).collect();
    turned_off.sort_unstable();
    let others: Vec<u64> = (0..u64::from(cpus)).filter(|&cpu| cpu != last && !left_on.contains(&cpu)).collect();
    if turned_off != others || calls[at..].iter().any(|&(function, _)| function == PSCI_CPU_OFF) {
        return Err(format!(
            "before {last:#x}'s SYSTEM_OFF, CPU_OFF came from {turned_off:x?}, not from each of {others:x?} \
             once and nothing after: {calls:x?}"
        ));
    }
    Ok(last)
}

/// `smp-skip-ap` keeps `CPU_ON` from the CPU that would be cpu2 of four, and
/// the bring-up stops there, so cpu2's id goes to no CPU behind it. The case's
/// job, which the scheduler places across the CPUs that came up, ends with
/// exit 0.
fn virt_failed_ap_leaves_no_hole(profile: qemu::Profile) -> Result<(), String> {
    const CPUS: u32 = 4;
    let mut qemu =
        boot_virt_smp(BootOptions { profile, smp: CPUS, kernel_params: &["smp-skip-ap"], ..Default::default() });
    let mut serial = virt_console(&qemu);
    judge_virt_job(&mut qemu, &mut serial, "unmap_touch", UNMAP_TOUCH_SAID)?;
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

/// `tests/virtrebootcase`'s one job asks for a reboot: the boot's last word is
/// `Rebooting.`, QEMU stops for `guest-reset`, and its trace of PSCI holds one
/// `SYSTEM_RESET` and neither `CPU_OFF` nor `SYSTEM_OFF`.
fn virt_reboot(profile: qemu::Profile) -> Result<(), String> {
    let config = compile::repo_root().join("tests/virtrebootcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    let trace = common::lane::dir().join("virt_reboot.psci");
    let mut qemu = QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            profile,
            qmp: true,
            psci_trace: Some(trace.clone()),
            ready_marker: "control registers: SCTLR_EL1=",
            ..Default::default()
        },
    );
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    let console = qemu.boot_log().to_string();
    let (console, calls) =
        ended_through_psci(&mut qemu, &mut stop, console, bootlog::REBOOTING, "guest-reset", &trace, |_| Vec::new())?;
    // The runner's own deadline also reboots, and is not the job asking.
    if console.contains(bootlog::JOB_DEADLINE_SAID) {
        return Err(format!("the reboot was the job list's deadline, not its job\n{console}"));
    }
    let asked: Vec<u64> = calls
        .iter()
        .map(|&(function, _)| function)
        .filter(|&function| [PSCI_CPU_OFF, PSCI_SYSTEM_OFF, PSCI_SYSTEM_RESET].contains(&function))
        .collect();
    if asked != [PSCI_SYSTEM_RESET] {
        return Err(format!("QEMU traced {asked:x?} of CPU_OFF, SYSTEM_OFF and SYSTEM_RESET, not one SYSTEM_RESET"));
    }
    eprintln!("  [virt] Rebooting., then one SYSTEM_RESET, and QEMU stopped for guest-reset");
    Ok(())
}

/// `power-off-spares-the-last-two`: cpu6 and cpu7 halt on the power-off's SGI
/// without `CPU_OFF`, so `AFFINITY_INFO` answers each on to the end of the
/// budget. Each of them but the one powering off is named after the last
/// word, in roster order, and nothing else is; every other CPU is off before
/// the one `SYSTEM_OFF`.
fn virt_off_names_the_cpus_left_on(profile: qemu::Profile) -> Result<(), String> {
    let trace = common::lane::dir().join("virt_off_left_on.psci");
    let mut qemu = boot_virt_smp(BootOptions {
        profile,
        smp: VIRT_CPUS,
        qmp: true,
        kernel_params: &["power-off-spares-the-last-two"],
        psci_trace: Some(trace.clone()),
        ..Default::default()
    });
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    let mut serial = virt_console(&qemu);
    judge_virt_job(&mut qemu, &mut serial, "unmap_touch", UNMAP_TOUCH_SAID)?;
    let spared = |calls: &[(u64, u64)]| -> Vec<u64> {
        let last = calls.iter().find(|&&(function, _)| function == PSCI_SYSTEM_OFF).map(|&(_, cpu)| cpu);
        (u64::from(VIRT_CPUS) - 2..u64::from(VIRT_CPUS)).filter(|&cpu| Some(cpu) != last).collect()
    };
    let named = |calls: &[(u64, u64)]| -> Vec<String> {
        spared(calls)
            .iter()
            .map(|cpu| format!("power: cpu{cpu} is not off by PSCI's answer inside the budget; SYSTEM_OFF regardless"))
            .collect()
    };
    let (_, calls) = ended_through_psci(&mut qemu, &mut stop, serial, power::SHUTTING_DOWN, "guest-shutdown", &trace, named)?;
    let left_on = spared(&calls);
    let last = psci_powered_off(&calls, VIRT_CPUS, &left_on)?;
    eprintln!(
        "  [virt] {left_on:?} left on and named; the rest CPU_OFF, then {last:#x} SYSTEM_OFF; {} PSCI call(s) traced",
        calls.len()
    );
    Ok(())
}

/// What the kernel says refusing a reboot on a machine with no reset.
const REBOOT_REFUSED: &str = "reboot: this machine has no reset this kernel performs — refused";

/// `psci-withheld` leaves this kernel no PSCI and so no reset:
/// `tests/virtrebootcase`'s job `reboot` is refused by name before anything is
/// torn down, ends with exit 1, and the boot never says `Rebooting.`.
fn virt_reboot_refused_without_psci(profile: qemu::Profile) -> Result<(), String> {
    let config = compile::repo_root().join("tests/virtrebootcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    let mut qemu = QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            profile,
            kernel_params: &["psci-withheld"],
            ready_marker: "control registers: SCTLR_EL1=",
            ..Default::default()
        },
    );
    let mut console = virt_console(&qemu);
    // A wait each: the job's end is test-runner's line and the refusal the
    // kernel's record, and the two reach the PL011 by different roads.
    await_marker(&mut qemu, &mut console, "===TEST_END reboot ", "the job reboot to end")?;
    await_marker(&mut qemu, &mut console, REBOOT_REFUSED, "the kernel's refusal of the reboot")
        .map_err(|why| format!("{why}\n{console}"))?;
    if !console.contains("===TEST_END reboot exit=1===") {
        return Err(format!("the job reboot did not end with exit 1\n{console}"));
    }
    if console.contains(bootlog::REBOOTING) {
        return Err(format!("a machine with no reset began one\n{console}"));
    }
    eprintln!("  [virt] {REBOOT_REFUSED}, and the job ended exit 1");
    Ok(())
}

/// `tests/virtpaniccase` runs `test_rs_panic_halts_first` as its one job on
/// [`STOP_CPUS`] CPUs, and the halt SGI stops every CPU but the one going
/// fatal.
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
    let rest = qemu.drain_until(Duration::from_secs(36), |l| l.contains(&said));
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
        "screen_panic_muted" => {
            // The machine the whole M0/M1 line exists for: metal-sim with the
            // 16550 taken away, so `uart_present()` is false, `panic_flush`
            // returns without draining anywhere, and the rendered screen is
            // the only channel the report can possibly reach. It is the one
            // place the absent-UART branches run at all.
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
            // `halt_all_cpus` refreshes to carry it.
            // The bound is derived: a panel promising a minute while the kernel
            // counts something else is the failure this line exists to catch.
            let armed = format!(
                "panic: rebooting in {} s, timed by ",
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
        "screen_loader_clears" => {
            // A loader's screen lasts until the kernel's first paint, which no
            // poll is sure to catch, so the machine is held where a loader pass
            // ends in a reset of its own: the pass after a reset that kept
            // memory reads the black box the first pass armed, and hands the
            // machine back.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                &[],
                &[],
                BootOptions { profile, qmp: true, ready_marker: bootlog::LOADER_LAST_LINE, ..Default::default() },
            );
            qemu.reset_and_hold_the_next(Duration::from_secs(30))?;
            let serial = qemu.drain_until(Duration::from_secs(5), |l| l.contains(bootlog::CHAIN_ENDS_LINE));
            if !serial.contains(bootlog::CHAIN_ENDS_LINE) {
                return Err(format!("the held boot is not a loader pass that ended its chain:\n{serial}"));
            }
            let dump = qemu.screendump();
            const BLACK: [u8; 3] = [0, 0, 0];
            // The firmware's logo is in colours its console's text is not.
            let colours: BTreeSet<[u8; 3]> = dump.pixels.iter().copied().collect();
            if colours.len() != 2 || !colours.contains(&BLACK) {
                return Err(format!(
                    "the loader's screen carries {} colours, not black and the console's text alone: \
                     something the firmware drew is still on it",
                    colours.len()
                ));
            }
            // The top line of ink is the loader's first, as wide as its
            // characters at the 8-pixel glyphs of EDK2's console: the firmware's
            // boot manager announces the option it starts on a line of its own.
            // A pass that reads a finding appends to its log, so its first line
            // is the separator; what precedes it on the 16550 is the terminal's
            // escapes for the clear.
            let Some(first) = serial.lines().find(|l| l.contains(bootlog::SEPARATOR)) else {
                return Err(format!("the held pass never wrote {:?}:\n{serial}", bootlog::SEPARATOR));
            };
            // An escape runs from ESC to its final letter.
            let shown = match first.rfind('\x1b') {
                Some(at) => {
                    let escape = &first[at..];
                    &escape[escape.find(|c: char| c.is_ascii_alphabetic()).map_or(escape.len(), |end| end + 1)..]
                }
                None => first,
            };
            let chars = shown.trim().len();
            let (width, pixels) = (dump.width, &dump.pixels);
            let inked = |y: usize| (0..width).filter(move |&x| pixels[y * width + x] != BLACK);
            // The first band of ink from `from` down: where it ends, and how wide it is.
            let band = |from: usize| -> Option<(usize, usize)> {
                let top = (from..dump.height).find(|&y| inked(y).next().is_some())?;
                let bottom = (top..dump.height).find(|&y| inked(y).next().is_none()).unwrap_or(dump.height);
                let xs: Vec<usize> = (top..bottom).flat_map(inked).collect();
                Some((bottom, xs.iter().max()? - xs.iter().min()? + 1))
            };
            let fits = |wide: usize, n: usize| n > 0 && wide > 8 * (n - 1) && wide <= 8 * n;
            let Some((bottom, wide)) = band(0) else {
                return Err("the loader's screen is blank".to_string());
            };
            // A line wider than the console wraps: its top row is the console's
            // width, and the row under it holds the rest of the line.
            let columns = if fits(wide, chars) { chars } else { wide.div_ceil(8) };
            let rest = chars.saturating_sub(columns);
            let under = band(bottom).map(|(_, wide)| wide);
            if !fits(wide, columns.min(chars)) || (rest > 0 && !under.is_some_and(|w| fits(w, rest.min(columns)))) {
                return Err(format!(
                    "the screen's top line is {wide} px wide and the one under it {under:?}, and the \
                     loader's first, {shown:?}, is {chars} characters: something else is above the \
                     loader's lines\n{serial}"
                ));
            }
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
            let dump = qemu.screendump_until("EARLY PANIC:", Duration::from_secs(6));
            let rest = qemu.drain_until(Duration::from_secs(6), |l| l.contains(EARLY_PANIC_MESSAGE));
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
            // loader that refuses the CPU says so and stops; a handover with
            // EL2's MMU on halts in a named refusal, or faults first where
            // firmware maps the image execute-never; a drop that leaves
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
                "CPU: entered at EL2, HCR_EL2.E2H 0,",
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
            let dump = qemu.screendump_until("EARLY PANIC:", Duration::from_secs(6));
            const FAULT_MESSAGE: &str = "synchronous from EL1 on SP_EL1: unknown reason (an undefined instruction) at 0x";
            let rest = qemu.drain_until(Duration::from_secs(6), |l| l.contains(FAULT_MESSAGE));
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
            // own tables, the GIC and the timer, and a process at EL0 — the supervisor,
            // whose every page arrives by a demand fault and whose spawn of
            // `logkeeper` is a syscall the kernel answered. Emulated, and not under
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
            const SPAWNED: &str = "spawn: /system/bin/logkeeper pid=";
            let rest = qemu.drain_until(Duration::from_secs(30), |l| l.contains(SPAWNED));
            let serial = format!("{}\n{rest}", qemu.boot_log());
            for want in [
                "paging: the direct map holds memory below",
                "percpu: BSP cpu_id=0",
                "GIC: v",
                "clock: the generic timer counts at",
                "spawned /system/bin/supervisor pid=",
                SPAWNED,
            ] {
                if !serial.contains(want) {
                    return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
                }
            }
            Ok(())
        }
        "virt_timer_preempts" => virt_job(profile, "preempt", "preempt: the counting thread was preempted twice"),
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
        "virt_ring0_timer_in_syscall" => virt_job(
            profile,
            &format!("test_rs_{VIRT_RING0_TIMER}"),
            "the timer interrupted the syscall's body and re-armed a quantum",
        ),
        "virt_mask_windows" => virt_mask_windows(profile),
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
        "virt_reboot" => virt_reboot(profile),
        "virt_off_names_the_cpus_left_on" => virt_off_names_the_cpus_left_on(profile),
        "virt_reboot_refused_without_psci" => virt_reboot_refused_without_psci(profile),
        "screen_fatal_behind_a_painter" => {
            // The fatal halt with a painter holding the panel's latch and
            // never giving it back — which is what a painter is when the halt
            // IPI lands mid-paint. The actuator has Ctrl+Alt+D's report painter
            // go fatal once it holds the latch, so the fatal path meets a
            // holder beneath itself; the report must take the screen
            // regardless, and its CPU must go on to watch the reset bound,
            // which is what the reset proves.
            const HELD: &str = "panel: a painter holding the panel went fatal";
            /// `panic_reboot::reboot_now`'s line, raw on the 16550, which is
            /// this profile's console.
            const BOUND_OVER: &str = "panic: the bound is over";
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                &[],
                &[],
                BootOptions {
                    profile,
                    qmp: true,
                    kernel_params: &["panel-painter-stalls", "panic-reboot-fast"],
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
            // Only the CPU that claimed the panel watches the reset bound, so
            // the machine resetting at it is that CPU's proof: QEMU exits on a
            // reset, and the fatal path's last line says whose reset it was.
            let by = qemu.budget(Duration::from_secs(15));
            let said = qemu.await_exit(by).map_err(|why| {
                format!("no CPU is watching the reset bound: {why}\ndecoded screen:\n{text}")
            })?;
            if !said.contains(BOUND_OVER) {
                return Err(format!(
                    "QEMU exited without {BOUND_OVER:?} on the console, so this was not the panic \
                     path's reset\n{said}"
                ));
            }
            Ok(())
        }
        "screen_fatal_halt_composited" => {
            // **Can a fatal panic reach the panel once a compositor owns the
            // scanout?** Three investigations into the T14 have rested on the
            // answer being yes. The owner pulled his stick, waited a minute,
            // and saw the desktop unchanged — which is what this test is for:
            // if the fatal path cannot paint over a claimed framebuffer, every
            // "nothing appeared on the panel" observation to date says nothing
            // about what the kernel did.
            // Driven by `metal-panic-probe`, which is the same kernel the owner
            // flashes: a gate that staged this with SYS_DEBUG would certify a
            // path his image does not contain.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/panelcase");
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
            // on the fill: every kernel paint fills with `FILL_BOOT`, so
            // anything else is userland holding the panel. Without this the
            // test would prove that a fatal panic paints a screen nobody had
            // taken.
            let up = qemu.screendump_while(Duration::from_secs(96), Duration::from_millis(200), |d| {
                d.fill() != FILL_BOOT
            });
            if up.fill() == FILL_BOOT {
                return Err("the compositor never took the screen".to_string());
            }

            // The probe fires 5 s after the claim; the poll is for that.
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
            // half a second to let `logkeeper` write it, and the stick either had it
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
            // The death's own census, which the seal writes as lines of its
            // own. Anchored at the line's start: the ring's tail under it can
            // carry a blocked-task dump's `irq: cpu0`, behind a record's stamp.
            for owed in ["irq: cpu0 ", "tlb: shootdowns="] {
                if !sealed.lines().any(|line| line.starts_with(owed)) {
                    return Err(format!(
                        "the panic's sealed record has no line that begins {owed:?}, so this \
                         death took no census of the machine\n{sealed}"
                    ));
                }
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
    // isa-parallel that nothing declared — and the NIC is enough to make netstack
    // claim a device on the machine whose whole point is that it has none.
    // None of them appears in argv, so this flag is the only observable form
    // of their absence here.
    if !argv.iter().any(|a| a == "-nodefaults") {
        return Err("metal-sim did not pass -nodefaults; QEMU's default-device pass is back".to_string());
    }
    Ok(())
}

/// The scanout's memory type, out of the three records that decide it.
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

/// netstack's stream count returns once connections that ended without their
/// client's close request are let go, a client that left a connection its
/// peer holds is counted gone, and a connection whose receive end the kernel
/// refuses netstack's watch of is reset. One host server here ends each
/// connection it accepts at once and one holds each; the guest's comparisons
/// are the verdict.
fn netstack_socket_churn() -> Result<(), String> {
    const JOB: &str = "netstack_socket_churn";
    let server = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("the host server: {e}"))?;
    let port = server.local_addr().map_err(|e| format!("the host server's port: {e}"))?.port();
    // Ends with the process: a guest that never dials leaves it in `accept`.
    thread::spawn(move || server.incoming().for_each(drop));
    let holding = holding_server()?;

    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/toyos-rust-tests"), JOB);
    let mut qemu = boot_netcase(&[], &[(JOB.to_string(), bin)], BootOptions::default())?;
    let result =
        qemu.run_test(&format!("test_rs_netstack_socket_churn {port} {holding}"), Duration::from_secs(120));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    if !result.stdout.lines().any(|l| l.trim_end().ends_with("netstack_socket_churn: ok")) {
        return Err(format!("the guest never said it was done:\n{}", result.stdout));
    }
    Ok(())
}

/// A host server that holds each connection it accepts and reads none of it,
/// for as long as the process lives: its port. A guest that never dials
/// leaves it in `accept`.
fn holding_server() -> Result<u16, String> {
    let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("the holding server: {e}"))?;
    let port = holder.local_addr().map_err(|e| format!("the holding server's port: {e}"))?.port();
    thread::spawn(move || holder.incoming().collect::<Vec<_>>());
    Ok(port)
}

/// Boot `tests/netcase` with these binaries staged, to netstack's lease: its
/// jobs name their peer by an address.
fn boot_netcase(
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    options: BootOptions,
) -> Result<QemuInstance, String> {
    const LEASED: &str = "netstack: DHCP: lease ";
    let case = compile::repo_root().join("tests/netcase");
    let mut qemu = QemuInstance::boot_with_options(&case, c_bins, rust_bins, options);
    let mut console = qemu.boot_log().to_string();
    await_marker(&mut qemu, &mut console, LEASED, "netstack's lease").map_err(|e| format!("{e}\n{console}"))?;
    Ok(qemu)
}

/// Have the guest's network carry what dials a loopback port of QEMU's
/// choosing to each of `guest_ports`: those host ports, in that order.
fn forwards_into(qemu: &QemuInstance, guest_ports: [u16; 2]) -> Result<[u16; 2], String> {
    const FORWARD: &str = "TCP[HOST_FORWARD]";
    let mut monitor = qemu::QmpMonitor::open(qemu.qmp_socket());
    for port in guest_ports {
        // `net0` is the NIC's backend in `common::qemu`'s argv.
        let said = monitor.human(&format!("hostfwd_add net0 tcp:127.0.0.1:0-:{port}"));
        if !said.trim().is_empty() {
            return Err(format!("QEMU forwards nothing to the guest's port {port}: {said}"));
        }
    }
    // A row is the protocol, a descriptor, the host's address and port, then
    // the guest's.
    let table = monitor.human("info usernet");
    let mut host_ports = [0u16; 2];
    for (host, guest) in host_ports.iter_mut().zip(guest_ports) {
        *host = table
            .lines()
            .map(|row| row.split_whitespace().collect::<Vec<_>>())
            .find(|row| row.first() == Some(&FORWARD) && row.get(5) == Some(&guest.to_string().as_str()))
            .and_then(|row| row[3].parse().ok())
            .ok_or_else(|| format!("QEMU names no host port forwarded to the guest's {guest}:\n{table}"))?;
    }
    Ok(host_ports)
}

/// libc's sockets as a C program uses them, on one boot of `tests/netcase`:
/// each of its C cases dials a host server that holds what it accepts, or
/// sends to one that answers each datagram with itself, at the address the
/// guest's network gives the host; and once `nodelay_kept` says its two
/// listeners wait, the host dials each through a port QEMU forwards. A case's
/// own comparisons are its verdict.
fn libc_sockets() -> Result<(), String> {
    const HOST: &str = "10.0.2.2";
    /// The ports `nodelay_kept` listens on in the guest, which nothing else
    /// on its boot binds.
    const LISTENERS: [u16; 2] = [7001, 7002];
    /// `nodelay_kept.c`'s `WAITING`.
    const WAITING: &str = "nodelay_kept: both listeners wait for a peer";
    let holding = holding_server()?;
    let echo = std::net::UdpSocket::bind(("127.0.0.1", 0)).map_err(|e| format!("the answering server: {e}"))?;
    let answering = echo.local_addr().map_err(|e| format!("the answering server's port: {e}"))?.port();
    // Ends with the process, as the holding server does.
    thread::spawn(move || {
        let mut datagram = [0u8; 64];
        while let Ok((len, from)) = echo.recv_from(&mut datagram) {
            echo.send_to(&datagram[..len], from).expect("answer a datagram");
        }
    });

    // The address first: every case names its peer by one.
    let [held, clear] = LISTENERS;
    let cases = [
        ("addr_order", holding.to_string()),
        ("nodelay_kept", format!("{holding} {held} {clear}")),
        ("sendto_unbound", answering.to_string()),
    ];
    let case = compile::repo_root().join("tests/netcase");
    let bins: Vec<(String, Vec<u8>)> = cases
        .iter()
        .map(|(name, _)| (name.to_string(), compile::link_toyos(&compile::compile_own_c(&case, name), name)))
        .collect();
    let mut qemu = boot_netcase(&bins, &[], BootOptions { qmp: true, ..Default::default() })?;
    let forwarded = forwards_into(&qemu, LISTENERS)?;
    // Held to the test's end: a peer gone before the case's `accept` fails its hand-over.
    let mut dialled = Vec::new();
    for (name, ports) in cases {
        let result =
            qemu.run_test_hooked(&format!("test_c_{name} {HOST} {ports}"), Duration::from_secs(120), WAITING, |_| {
                dialled.extend(forwarded.map(|port| std::net::TcpStream::connect(("127.0.0.1", port))));
            });
        if let Some(Err(e)) = dialled.iter().find(|dial| dial.is_err()) {
            return Err(format!("{name}: the host could not dial a port QEMU forwards to the guest: {e}"));
        }
        if qemu::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("  [libc] {name} ended {:?} and said:\n{}", result.exit_code, result.stdout);
        }
        if let Some(why) = &result.error {
            return Err(format!("{name}: {why}\nthe case said:\n{}", result.stdout));
        }
        if result.exit_code != Some(0) {
            return Err(format!("{name} ended {:?}:\n{}", result.exit_code, result.stdout));
        }
        if !result.stdout.lines().any(|l| l.trim_end().ends_with(&format!("{name}: ok"))) {
            return Err(format!("{name} never said it was done:\n{}", result.stdout));
        }
    }
    Ok(())
}

/// Run the machine-shape test, which owns its QEMU: the machine shape *is* the
/// test.
fn run_machine_test(name: &str, test_config: &Path) -> Result<(), String> {
    match name {
        "iommu_virtio_platform" => common::iommu::iommu_virtio_platform(test_config),
        "netstack_socket_churn" => netstack_socket_churn(),
        "libc_sockets" => libc_sockets(),
        "nested_nmi_is_loud" => faults::nested_nmi_is_loud(test_config),
        "machine_shutdown" => power::machine_shutdown(test_config),
        "acpi_power_button" => power::acpi_power_button(test_config),
        "acpi_mediated_access" => acpi_mediated_access(2),
        "acpi_lock_given_back_on_one_cpu" => acpi_mediated_access(1),
        "acpi_supply_outlives_holder" => acpi_supply_outlives_holder(),
        "machine_shutdown_short_stop" => power::machine_shutdown_short_stop(test_config),
        "bar_map_again" => bar_map_again(test_config),
        "console_image_boots" => console_image_boots(),
        other => Err(format!("unknown machine test {other}")),
    }
}

/// A claim's memory BAR asked for again — while an earlier answer is held, and
/// once its handle is gone, closed or never installed for want of room: each
/// time the kernel answers a handle whose mapping reads the function's own
/// register, two held at once map apart, and the job that asked ends on its
/// own.
fn bar_map_again(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "bar_map_again";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/toyos-rust-tests"), JOB);
    let mut qemu =
        QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_bar_map_again", Duration::from_secs(60));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    for answered in [
        "bar_map_again: two answers held at once map apart, and the later outlives the earlier",
        "bar_map_again: answered with a handle that maps",
        "bar_map_again: answered after the refusal with a handle that maps",
    ] {
        let Some(line) = result.stdout.lines().find(|l| l.contains(answered)) else {
            return Err(format!("the job never said `{answered}`:\n{}", result.stdout));
        };
        eprintln!("  [claims] {}", line.trim());
    }
    Ok(())
}

/// Every device delivery is still cpu0's, and no CPU took a shootdown IPI the
/// issuer did not count.
///
/// **The capture is in stamp order, not read order** (`Census::raise`), of a
/// CPU's census and of the issuer's total alike, and no line says when it was
/// read. No two lines are compared by their place in the capture: a CPU's
/// census is the largest count each of its sources reached on any line, and the
/// issuer's is the largest `shootdowns=`.
fn irq_census(capture: &str) -> Result<(), String> {
    use common::irqcensus::{Census, DEVICE_SOURCES};
    let mut newest: BTreeMap<u32, Census> = BTreeMap::new();
    for line in capture.lines() {
        let census = match Census::parse(line) {
            None => continue,
            Some(Ok(census)) => census,
            Some(Err(why)) => return Err(format!("{why}\nline: {line}")),
        };
        newest.entry(census.cpu).or_insert_with(|| census.clone()).raise(&census);
    }
    if newest.is_empty() {
        return Err(format!(
            "no `irq: cpu` census in the capture — the machine stopped and the kernel \
             said nothing:\n{capture}"
        ));
    }

    // 1. The machine is real: the boot CPU took interrupts, and so did
    //    at least one AP — otherwise (2) says nothing.
    let cpu0 = newest
        .get(&0)
        .ok_or_else(|| format!("no cpu0 in the census: {newest:?}"))?;
    if cpu0.total() == 0 {
        return Err(format!("cpu0 took no interrupts at all: {cpu0:?}"));
    }
    let aps: Vec<&Census> = newest.values().filter(|c| c.cpu != 0).collect();
    if aps.len() < 3 {
        return Err(format!(
            "a 4-CPU machine reported {} AP(s); the census cannot see them all: {newest:?}",
            aps.len()
        ));
    }
    if !aps.iter().any(|c| c.total() > 0) {
        return Err(format!("no AP took a single interrupt: {newest:?}"));
    }

    // 2. **The present-state fact this whole track is about.** Every
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

    let share = cpu0.total() as f64
        / newest.values().map(Census::total).sum::<u64>() as f64
        * 100.0;
    eprintln!(
        "  [irq] {} cpu(s), {} interrupt(s), {delivered} of them device deliveries — \
         all on cpu0, which took {share:.1}% of everything",
        newest.len(),
        newest.values().map(Census::total).sum::<u64>(),
    );

    // 3. The issuer side: every `tlb` delivery a CPU's census carries
    //    must be within the issues a `tlb:` line counted — an excess
    //    is a path shooting down uncounted. The lower bound is not
    //    asserted: an issued IPI can be pending on an IF-clear target.
    //    The bound is the largest count: the stop reads the deliveries
    //    before the issuer's total.
    let mut issued: Option<u64> = None;
    for line in capture.lines() {
        let Some(rest) = line.split("tlb: shootdowns=").nth(1) else { continue };
        let n: u64 = rest
            .split_whitespace()
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("unreadable issuer census: {line}"))?;
        issued = issued.max(Some(n));
        eprintln!("  [tlb] {}", line.trim());
    }
    let Some(issued) = issued else {
        return Err(format!(
            "no `tlb: shootdowns=` census in the capture — the machine stopped and \
             the issuer side said nothing:\n{capture}"
        ));
    };
    for census in newest.values() {
        if census.source("tlb") > issued {
            return Err(format!(
                "cpu{} took {} tlb IPI(s) against {issued} counted issue(s) — \
                 some path shoots down without being counted: {census:?}",
                census.cpu,
                census.source("tlb"),
            ));
        }
    }
    eprintln!(
        "  [tlb] {issued} shootdown(s) issued, deliveries per CPU {:?} — every \
         delivery accounted for",
        newest.values().map(|c| c.source("tlb")).collect::<Vec<_>>(),
    );
    Ok(())
}

/// The T14's windows: [`mask_windows`]' verdict with every CPU reporting, and
/// `common::irqcensus::windows_under`'s. Its durations are printed and none is
/// recorded: one boot's longest window is no baseline for the next.
fn windows_on_metal(boot: &metal::Readback) -> Result<(), String> {
    boot.job_passed(WINDOWS_LOAD)?;
    let kernel = boot.kernel();
    let cpus = boot.cpus()?;
    mask_windows(kernel.text(), cpus)?;
    let read = common::irqcensus::windows_under(kernel.text(), cpus, &windows_load_exited())?;
    eprintln!(
        "  [windows] held irqs_off_ns={} preempt_off_ns={}; herd irqs_off_ns={} preempt_off_ns={}",
        read.held.0, read.held.1, read.load.0, read.load.1
    );
    Ok(())
}

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

/// `process_tree` passed, and the kernel said the two things the guest cannot
/// see: each B's first spawn was refused by the loader, past the admission that
/// holds B, and the chain's one depth refusal names `MAX_DEPTH` + 1.
fn process_tree(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_process_tree")?;
    let kernel = back.kernel();
    let log = kernel.text();
    let unloaded = log.lines().filter(|l| l.contains("spawn: /system/bin/no_such_program: ")).count();
    if unloaded != 2 {
        return Err(format!(
            "the loader refused /system/bin/no_such_program {unloaded} times, not once per B"
        ));
    }
    let refused: Vec<&str> = log.lines().filter(|l| l.contains("spawn: refused under pid ")).collect();
    if refused.len() != 1 || !refused[0].contains("at depth 65, more than 64 below the supervisor") {
        return Err(format!("the kernel's depth refusals were {refused:?}, not one naming depth 65"));
    }
    Ok(())
}

/// `launch_authority` passed, and the supervisor refused each launch for its
/// own reason: `proctest` as unlisted, `swap` and `update` as outside a login
/// session, for test-runner and for the toybox it launched. The guest sees
/// only that each was refused.
fn launch_authority(back: &metal::Readback) -> Result<(), String> {
    use toyos_manifest::launch::{refused, Refusal, Sessions};
    back.job_passed("test_rs_launch_authority")?;
    let log = back.log();
    for (caller, target, why) in [
        ("test-runner", "proctest", Refusal::NotListed),
        ("test-runner", "swap", Refusal::OutsideLogin),
        ("test-runner", "update", Refusal::OutsideLogin),
        ("toybox", "swap", Refusal::OutsideLogin),
    ] {
        let line = refused(caller, Sessions::default().machine(), target, why);
        if !log.text().lines().any(|l| l.contains(&line)) {
            return Err(format!("the supervisor never said `{line}`\n{}", log.text()));
        }
    }
    Ok(())
}

/// A backing read after deletion is refused on both writable mounts, and a page-cache slot whose fill the device refused is unbound.
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

/// `kernel/src/arch/x86_64/hw.rs`'s probe, when it could not run.
const SYSRET_SS_UNARMED: &str = "sysret-ss: probe could not arm";
/// The probe, when a switch refreshed SS from null.
const SYSRET_SS_RELOADED: &str = "sysret-ss: reloaded";
/// The probe, when SS stayed null across a switch.
const SYSRET_SS_NOT_RELOADED: &str = "sysret-ss: NOT reloaded";

/// The context switch reloads SS from null before a `sysretq` can see it.
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
fn input_merge_ok(log: &str) -> Result<(), String> {
        if !log.contains("input-merge: ok") {
            return Err(format!("the input core check never reported:\n{log}"));
        }
        Ok(())
}

/// Every unit this kernel went on to program said it was handed over with what
/// the actuator left on.
fn iommu_firmware_left(log: &str) -> Result<(), String> {
        let mut units = 0;
        for line in log.lines().filter(|l| l.contains(" translating gsts=")) {
            let unit = line
                .split("iommu: ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .ok_or_else(|| format!("an unreadable unit line: {line:?}"))?;
            let remaps = log
                .lines()
                .filter(|l| l.contains(&format!("iommu: {unit} @")))
                .find_map(|l| common::iommu::unit_fields(l).remove("ir"))
                .ok_or_else(|| format!("{unit} translates and no line describes its ir:\n{log}"))?
                == "y";
            let left = ["translation", "queued invalidation"]
                .into_iter()
                .chain(remaps.then_some("interrupt remapping"));
            for field in left {
                if !log.contains(&format!("iommu: {unit} was handed over with {field} on")) {
                    return Err(format!(
                        "{unit} translates and never said it was handed over with {field} on, so \
                         the state the actuator left was never switched off by the hand-over:\n{log}"
                    ));
                }
            }
            units += 1;
        }
        if units == 0 {
            return Err(format!("no unit was programmed, so nothing was handed over:\n{log}"));
        }
        let passed = log.matches("handed over with compatibility-format pass-through on").count();
        eprintln!(
            "  [iommu-firmware-left] {units} unit(s) programmed after switching off what they were \
             handed over with; {passed} of them reported compatibility format passed"
        );
        Ok(())
}

/// An inner `scheduler::Operation` may only narrow, and its drop restores what it displaced.
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
fn klogd_hosted(boot: &serial::Serial) -> Result<(), String> {
    boot.must_be_clean()?;
    let line = boot.must_say("kthread: klogd")?;
    eprintln!("  [kthread] {}", line.trim());
    Ok(())
}

/// Every I/O APIC this machine has, and whether its redirection table is a
/// chip's rather than a floating bus's.
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

/// The shortest span `counters_on_metal` reads the SMI count across: twice the
/// longest period between the T14's firmware interrupts that has been read
/// (`issues/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`).
const SMI_SPAN_NS: u64 = 4_444_000_000;

/// `counters_metal`'s three reads on the T14, held to what the hardware and
/// the bring-up said.
///
/// Held: a record per CPU the bring-up started, naming the local APIC id the
/// bring-up gave that CPU and carrying the counters its `counters: cpuN
/// reads` line names, and none stale; every CPU's performance request
/// declared at boot, `pm_enable=1`, the request Linux makes on this machine
/// (`tests/t14-linux/hwp-request.txt`), and its power envelope in every read
/// the one its `control_regs:` line holds. No line, the kernel's or a
/// program's, is stamped in a millisecond from `idle0`'s to `idle1`'s, which
/// `counters_metal` reads on the log's clock, either
/// edge's included because a line stamped in it may follow the read: the
/// second is the idle machine's. The boot ran in ACPI mode, which
/// `/system/bin/acpiserver`'s claim put it in: `idle0` reads after the
/// kernel's write of the enable to `SMI_CMD`, which the boot processor
/// counted with the time it held it, and no other CPU counts one; and from
/// there to `spin`, at least [`SMI_SPAN_NS`] apart, every CPU's SMI count
/// moves by the commands the kernel wrote to `SMI_CMD` in that span and by
/// nothing else: every firmware interrupt is one ToyOS asked for. Across the spin every
/// CPU's MPERF ran nine tenths of its stamp or more [e], a CPU in C0 the whole
/// span: MPERF counts at the TSC's rate there (SDM Vol. 3B, "Hardware
/// Coordination Feedback"); and every CPU's busy frequency reached the lowest
/// Linux's turbostat read under one `yes` per CPU on the same machine over the
/// same span of load (`tests/t14-linux/turbostat-loaded.txt`).
///
/// Read and not held, beside Linux's turbostat: each CPU's idle busy
/// fraction, which an SMI in the idle second raises on every CPU alike by the
/// time it held them, and what one round cost its reader; and beside Linux's loaded
/// timer reading (`issues/toyos-beats-linuxs-latency-on-the-t14.md`),
/// how late each CPU's kick handler ran under the `loaded` phase.
fn counters_on_metal(back: &metal::Readback) -> Result<(), String> {
    type Read<'a> = BTreeMap<usize, BTreeMap<&'a str, u64>>;
    back.job_passed("test_rs_counters_metal")?;
    let log = back.log();
    let kernel = back.kernel();
    let cpus = back.cpus()? as usize;
    let mut reads: BTreeMap<&str, Read> = BTreeMap::new();
    let mut clock: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for line in log.text().lines() {
        let Some((phase, said)) = line.split("counters_metal ").nth(1).and_then(|r| r.split_once(": ")) else {
            continue;
        };
        let number = |s: &str| s.trim().parse::<u64>().map_err(|_| format!("{s:?} in {line:?} is not a number"));
        if let Some(rest) = said.strip_prefix("at ") {
            let (at, took) = rest.split_once(" ns, the read took ").ok_or_else(|| format!("unreadable: {line}"))?;
            clock.insert(phase, (number(at)?, number(took.trim_end_matches(" ns"))?));
        } else if let Some(rest) = said.strip_prefix("kernel.cpu.") {
            let (path, value) = rest.split_once(" = ").ok_or_else(|| format!("unreadable: {line}"))?;
            let (cpu, name) = path.split_once('.').ok_or_else(|| format!("unreadable: {line}"))?;
            let value = match value {
                "true" => 1,
                "false" => 0,
                v => number(v)?,
            };
            reads.entry(phase).or_default().entry(number(cpu)? as usize).or_default().insert(name, value);
        }
    }
    let phase = |name: &str| -> Result<(&Read, u64), String> {
        let read = reads.get(name).ok_or_else(|| format!("counters_metal printed no {name} read"))?;
        Ok((read, clock.get(name).ok_or_else(|| format!("counters_metal printed no {name} clock"))?.0))
    };
    let (idle0, at0) = phase("idle0")?;
    let (idle1, at1) = phase("idle1")?;
    let (spin, at2) = phase("spin")?;
    let (from_ms, to_ms) = (at0 / 1_000_000, at1 / 1_000_000);
    let inside: Vec<&str> = log
        .text()
        .lines()
        .filter(|line| {
            toyos_logstream::parse(line).and_then(|p| p.ms).is_some_and(|ms| (from_ms..=to_ms).contains(&ms))
        })
        .collect();
    if !inside.is_empty() {
        return Err(format!("the idle second {at0}..{at1} ns holds lines: {inside:?}"));
    }
    let linux_request = u64::from_str_radix(include_str!("t14-linux/hwp-request.txt").trim().trim_start_matches("0x"), 16)
        .map_err(|e| format!("t14-linux/hwp-request.txt: {e}"))?;
    let bsp = kernel.must_say("percpu: BSP cpu_id=0 lapic_id=")?;
    let mut roster = vec![bsp.rsplit("lapic_id=").next().unwrap_or_default().trim().to_string()];
    for cpu in 1..cpus {
        roster.push(field_between(kernel.text(), &format!("SMP: AP cpu{cpu} lapic="), " online")?.to_string());
    }
    for (name, read) in [("idle0", idle0), ("idle1", idle1), ("spin", spin)] {
        if read.keys().copied().ne(0..cpus) {
            return Err(format!("{name} read cpus {:?}, and the bring-up started {cpus}", read.keys()));
        }
        for (cpu, counters) in read {
            let reads = kernel.must_say(&format!("counters: cpu{cpu} reads "))?;
            for counter in ["smi", "aperf", "mperf", "hwp_request", "hwp_request_pkg", "energy_perf_bias"] {
                if !reads.contains(&format!("{counter}={}", counters.contains_key(counter))) {
                    return Err(format!("{name}: cpu{cpu} carries {counters:?} and its bring-up said {reads:?}"));
                }
            }
            if counters.get("stale") != Some(&0) || counters.get("hardware_id").map(u64::to_string) != Some(roster[*cpu].clone()) {
                return Err(format!("{name}: cpu{cpu} is stale or not lapic {}: {counters:?}", roster[*cpu]));
            }
            let declared = kernel.must_say(&format!("control_regs: cpu{cpu} pm_enable=1 "))?;
            if !declared.contains(&format!(" hwp_request={linux_request:#010x} ")) {
                return Err(format!("cpu{cpu} declared {declared:?}, and Linux requests {linux_request:#010x} on this machine"));
            }
            for (counter, field, radix) in
                [("hwp_request", "hwp_request=0x", 16), ("hwp_request_pkg", "hwp_request_pkg=0x", 16), ("energy_perf_bias", "epb=", 10)]
            {
                let held = declared
                    .split(' ')
                    .find_map(|w| u64::from_str_radix(w.strip_prefix(field)?, radix).ok())
                    .ok_or_else(|| format!("cpu{cpu}'s declaration carries no {field}: {declared:?}"))?;
                if counters.get(counter) != Some(&held) {
                    return Err(format!("{name}: cpu{cpu} reads {counter} {:?} and boot declared {held}: {declared:?}", counters.get(counter)));
                }
            }
        }
    }
    for cpu in 0..cpus {
        for (name, &value) in &idle1[&cpu] {
            if idle0[&cpu][name] > value || value > spin[&cpu][name] {
                return Err(format!("cpu{cpu}'s {name} went backwards: {idle0:?} {idle1:?} {spin:?}"));
            }
        }
    }
    if at2 - at0 < SMI_SPAN_NS {
        return Err(format!("idle0 and spin are {} ns apart, short of {SMI_SPAN_NS}: lengthen the spin", at2 - at0));
    }
    let delta = |a: &Read, b: &Read, cpu: usize, name: &str| b[&cpu][name] - a[&cpu][name];
    // The enable's write to `SMI_CMD`, and what the CPU that made it read after it.
    let enabled = kernel.must_say("acpi: ACPI mode: ACPI_ENABLE ")?;
    let writer = smi_cmd_writer(enabled)?;
    let after = number_between(enabled, " before the write and ", " after")?;
    if idle0[&writer]["smi"] < after {
        return Err(format!("idle0 read cpu{writer}'s SMI count below what it read after the ACPI enable ({after}): it read before that write to SMI_CMD"));
    }
    if let Ok(left) = kernel.must_say("acpi: legacy mode again") {
        return Err(format!("the machine left ACPI mode inside the boot: {left}"));
    }
    // The boot processor writes every command and alone counts them: the
    // enable is among those idle0 read, and where it is the only one, the
    // time they held that CPU is the time the kernel's line gives it.
    if let Some(cpu) = (0..cpus).find(|&cpu| idle0[&cpu].contains_key("firmware_calls") != (cpu == writer)) {
        return Err(format!("cpu{cpu} counts firmware calls or the boot processor does not: {idle0:?}"));
    }
    let (calls, nanos) = (idle0[&writer]["firmware_calls"], idle0[&writer]["firmware_nanos"]);
    let held = number_between(enabled, "; the write held cpu0 ", "ns, its SMI count ")?;
    if calls == 0 || nanos < held || (calls == 1 && nanos != held) {
        return Err(format!("idle0 read {calls} firmware calls that held cpu{writer} {nanos} ns, after an enable that held it {held} ns: {enabled}"));
    }
    let asked = delta(idle0, spin, writer, "firmware_calls");
    let smis: Vec<u64> = (0..cpus).map(|cpu| delta(idle0, spin, cpu, "smi")).collect();
    if smis.iter().any(|&n| n != asked) {
        return Err(format!(
            "in ACPI mode the SMI count moved by {smis:?} over {} ns, in which the kernel wrote {asked} commands to SMI_CMD",
            at2 - at0
        ));
    }
    let firsts: Vec<u64> = (0..cpus).map(|cpu| idle0[&cpu]["smi"]).collect();
    eprintln!("  [counters] {}", enabled.trim());
    eprintln!("  [counters] idle0's SMI count per cpu {firsts:?}, the writer's after the enable {after} (a reading)");
    eprintln!("  [counters] idle0 read {calls} firmware call(s) that held cpu{writer} {nanos} ns, the enable's {held} ns among them");
    let tsc_mhz = delta(idle0, spin, 0, "stamp") as f64 * 1e3 / (at2 - at0) as f64;
    let ratio = |a: &Read, b: &Read, cpu: usize, top: &str, bottom: &str| {
        delta(a, b, cpu, top) as f64 / delta(a, b, cpu, bottom) as f64
    };
    let busy: Vec<f64> = (0..cpus).map(|cpu| ratio(idle1, spin, cpu, "mperf", "stamp")).collect();
    if busy.iter().any(|&b| b < 0.9) {
        return Err(format!("MPERF ran {busy:?} of each stamp across the spin, some cpu under 0.9"));
    }
    // Each interval's machine-wide row, in order.
    let linux = |file: &str, column: &str| -> Result<Vec<f64>, String> {
        let mut rows = file.lines().map(|l| l.split('\t').collect::<Vec<_>>());
        let header = rows.next().ok_or("an empty turbostat reading")?;
        let at = header.iter().position(|c| *c == column).ok_or_else(|| format!("turbostat read no {column}"))?;
        Ok(rows.filter(|r| r[0] == "-").filter_map(|r| r.get(at)?.parse().ok()).collect())
    };
    log.must_say("counters_metal loaded: ")?;
    for said in log.text().lines().filter(|l| l.contains("counters_metal loaded: ")) {
        eprintln!("  [counters] {}", said.split("counters_metal ").nth(1).unwrap_or(said).trim());
    }
    let range = |values: &[f64]| values.iter().fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let idle = range(&linux(include_str!("t14-linux/turbostat-idle.txt"), "Busy%")?);
    let loaded_rows = linux(include_str!("t14-linux/turbostat-loaded.txt"), "Bzy_MHz")?;
    let loaded = range(&loaded_rows);
    // The floor is Linux's clock over the spin's own span of load, not after
    // the package settles: the intervals that open within it, the first 2 s
    // into Linux's load and each 10 s long (`tests/t14-linux/SOURCE`).
    let spin_ns = at2 - at1;
    let opened = loaded_rows.iter().enumerate().take_while(|&(k, _)| 2_000_000_000 + k as u64 * 10_000_000_000 < spin_ns);
    let floor = opened.map(|(_, &mhz)| mhz).fold(f64::MAX, f64::min);
    if floor == f64::MAX {
        return Err(format!("the spin lasted {spin_ns} ns, and no Linux interval opens within it"));
    }
    let spinning: Vec<f64> = (0..cpus).map(|cpu| tsc_mhz * ratio(idle1, spin, cpu, "aperf", "mperf")).collect();
    eprintln!(
        "  [counters] {cpus} cpus, the SMI count on each moved by {asked} over {} ms, the commands the kernel wrote to SMI_CMD in it; TSC {tsc_mhz:.0} MHz",
        (at2 - at0) / 1_000_000
    );
    for (cpu, busy) in busy.iter().enumerate() {
        eprintln!(
            "  [counters] cpu{cpu}: idle busy {:.2}% (Linux {:.2}-{:.2}%), spinning {:.0} MHz (Linux {floor:.0} over \
             this span, {:.0}-{:.0} loaded), busy {busy:.3}",
            ratio(idle0, idle1, cpu, "mperf", "stamp") * 100.0,
            idle.0,
            idle.1,
            spinning[cpu],
            loaded.0,
            loaded.1,
        );
    }
    eprintln!("  [counters] the spin's read, a whole round, took its reader {} ns", clock["spin"].1);
    if spinning.iter().any(|&mhz| mhz < floor) {
        return Err(format!("spinning {spin_ns} ns at {spinning:.0?} MHz, some cpu below the {floor:.0} Linux held over that span"));
    }
    Ok(())
}

/// The T14's ACPI row as the kernel filled it from the machine's FACP, ECDT
/// and APIC (Linux on the same machine: `EC_CMD/EC_SC=0x66, EC_DATA=0x62`,
/// `GPE=0x6e`, `INT_SRC_OVR (bus 0 bus_irq 9 global_irq 9 high level)`), the
/// server armed on it, at least one embedded-controller query taken and a
/// count of them logged, and no guard of the server's fired.
fn acpi_events_on_metal(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_acpi_hold")?;
    let (log, kernel) = (back.log(), back.kernel());
    let enabled = kernel.must_say("acpi: ACPI mode: ACPI_ENABLE 0xf0 written to SMI_CMD 0xb2 ")?;
    smi_cmd_writer(enabled)?;
    eprintln!("  [acpi] {}", enabled.trim());
    kernel.must_say(
        "acpi: the ACPI row: PM1a events 0x1800+4, GPE0 0x1860+32, SCI gsi 9 level/high, the \
         fixed-hardware power button, embedded controller at 0x66/0x62 on GPE 0x6e; the firmware \
         handed over in legacy mode",
    )?;
    log.must_say(
        "acpiserver: armed: power button served, embedded controller on GPE 0x6e at 0x66/0x62",
    )?;
    let lines: Vec<&str> = log
        .text()
        .lines()
        .filter(|l| toyos_logstream::program_line(l).is_some_and(|said| said.tag == "acpiserver"))
        .collect();
    if let Some(fired) = lines.iter().find(|l| l.contains("panicked")) {
        return Err(format!("the server died: {fired}"));
    }
    let firsts: Vec<&&str> = lines.iter().filter(|l| l.contains("taken for the first time")).collect();
    let counts = lines.iter().rfind(|l| l.contains(acpiserver_api::QUERIES_COUNTED));
    let (true, Some(counts)) = (!firsts.is_empty(), counts) else {
        return Err(format!("the server logged {} first sighting(s) and {counts:?} for counts", firsts.len()));
    };
    for first in firsts {
        eprintln!("  [acpi] {}", first.trim());
    }
    eprintln!("  [acpi] {}", counts.trim());
    Ok(())
}

/// How many definition blocks the T14 has: Linux on the same machine says
/// `14 ACPI AML tables successfully acquired and loaded`.
const T14_DEFINITION_BLOCKS: usize = 14;

/// The kernel's line for the sleep type its server handed it, whole to the
/// value, on the T14: that machine's PM1a control block, and its `\_S5`'s
/// `SLP_TYPa` as a byte scan of its DSDT reads it, with no interpreter.
const T14_S5_SUPPLIED: &str = "power: S5 is PM1a 0x1804 with SLP_TYPa=7,";

/// The server's load of the T14's tables, every access the kernel's to make
/// for it: all of the machine's definition blocks, as many as Linux loads
/// there, each fetched through `SYS_ACPI`, summing to zero as its firmware
/// sealed it, and loaded; `\_S5`'s `SLP_TYPa` handed to the kernel, which
/// says it powers this machine off with that value on its own PM1a block,
/// both as [`T14_S5_SUPPLIED`] holds them, and never does on this row; and
/// nothing refused, so no bridge answered
/// what the interpreter refuses, no address was `Unmapped`, and no access
/// the load makes is one the policy keeps from it. The load's AML read
/// memory, read configuration space and took the Global Lock, which is the
/// real lock word exchanged and given back each time. Every other CPU of the
/// machine said how its range registers stand beside the boot processor's,
/// which typed the load's unlisted read a register's, and none holds registers
/// that are on and not those. What it prints beside
/// that is the first measurement of each: the load's time, the reads by
/// address space, the takes that found the firmware holding the lock, and
/// the pages of memory by the type the firmware's map gives them.
fn acpi_tables_on_metal(back: &metal::Readback) -> Result<(), String> {
    let (log, kernel) = (back.log(), back.kernel());
    let lines: Vec<&str> = log
        .text()
        .lines()
        .filter(|l| toyos_logstream::program_line(l).is_some_and(|said| said.tag == "acpiserver"))
        .collect();
    if let Some(fired) = lines.iter().find(|l| l.contains("panicked")) {
        return Err(format!("the server died: {fired}"));
    }
    let blocks = power::acpi_tables_loaded(&log, &kernel, T14_S5_SUPPLIED)?;
    if blocks != T14_DEFINITION_BLOCKS {
        return Err(format!("the server found {blocks} definition blocks where Linux loads {T14_DEFINITION_BLOCKS}"));
    }
    let others = number_between(kernel.text(), "SMP: ", " of ")? - 1;
    let compared: Vec<&str> = kernel.text().lines().filter(|l| l.contains("mtrr: cpu") && l.contains("'s range registers are ")).collect();
    if compared.len() as u64 != others || compared.iter().any(|l| l.contains("are on and not the boot processor's")) {
        return Err(format!(
            "{others} other CPUs came up, and their range registers beside the boot processor's are {compared:#?}"
        ));
    }
    let off = compared.iter().filter(|l| l.contains("range registers are off")).count();
    eprintln!("  [acpi] {others} other CPUs' range registers compared with the boot processor's: {off} off, none on and different");
    let refused: Vec<&&str> = lines.iter().filter(|l| l.contains("acpiserver: refused") || l.contains(" refused: ")).collect();
    if !refused.is_empty() {
        return Err(format!("the server refused something of this machine's AML: {refused:#?}"));
    }
    let took = log.must_say(&format!("acpiserver: {blocks} of {blocks} tables loaded in "))?;
    let bytes = log.must_say("acpiserver: the tables' bytes took ")?;
    let aml = log.must_say("acpiserver: the tables' AML read SystemMemory ")?;
    let memory = number_between(aml, "AML read SystemMemory ", " times, SystemIO ")?;
    let config = number_between(aml, " and PCI_Config ", ", its memory in pages: ")?;
    let takes = number_between(aml, "; took the Global Lock ", " times, ")?;
    if memory == 0 || config == 0 || takes == 0 {
        return Err(format!("this machine's tables read memory and configuration space and take the Global Lock as they load, and the server's did not: {aml}"));
    }
    for line in [took, bytes, aml] {
        eprintln!("  [acpi] {}", line.trim());
    }
    Ok(())
}

/// The server's death on the T14: the kernel put the machine in ACPI mode for
/// the job's claim, and when the killed server's claim went it wrote
/// `ACPI_DISABLE` (the FADT's 0xf1) and read `SCI_EN` clear: the firmware has
/// the buttons again. Each write was made on the boot processor, and the
/// enable was asked from another CPU: the job claims from a thread it found
/// off the boot processor, so that write is the one that crossed. And the
/// Global Lock this machine's firmware keeps: the kernel read the FACS its
/// FADT names through the direct map and found the lock word in ACPI NVS
/// memory (type 10) by the firmware's own map, which is where Linux's print
/// of the same map puts it.
fn acpi_death_on_metal(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_acpi_release")?;
    let kernel = back.kernel();
    let lock = kernel.must_say("acpi: the Global Lock is the FACS's at ")?.trim();
    if !lock.ends_with(", in memory the firmware's map types 10") {
        return Err(format!("the Global Lock's word is not in ACPI NVS memory: {lock}"));
    }
    eprintln!("  [acpi] {lock}");
    let enabled = kernel.must_say("acpi: ACPI mode: ACPI_ENABLE 0xf0 written to SMI_CMD 0xb2 ")?;
    // The kernel says this only of a `PM1a_CNT` it read with `SCI_EN` clear.
    let left = kernel.must_say("acpi: legacy mode again: ACPI_DISABLE 0xf1 written to SMI_CMD 0xb2 ")?;
    for line in [enabled, left] {
        smi_cmd_writer(line)?;
        eprintln!("  [acpi] {}", line.trim());
    }
    if number_between(enabled, ", asked from cpu", "; the write held cpu")? == 0 {
        return Err(format!("the job's claim was asked from the boot processor, so no CPU asked it for this write: {enabled}"));
    }
    Ok(())
}

/// The CPU a kernel line says wrote `SMI_CMD`, read on that CPU beside the
/// `out`: the boot processor, cpu0, or the line is refused (ACPI 6.5 Table
/// 5.9, "from the boot processor").
fn smi_cmd_writer(line: &str) -> Result<usize, String> {
    let writer = number_between(line, " written to SMI_CMD 0xb2 on cpu", ", asked from cpu")? as usize;
    if writer != 0 {
        return Err(format!("SMI_CMD was written on cpu{writer}, not on the boot processor: {line}"));
    }
    Ok(writer)
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
    for said in boot.log().text().lines().filter(|l| l.contains("cyclictest: ")) {
        eprintln!("  [latency] {}", said.trim());
    }
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
        "target/{}/{}/kernel",
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
/// one [`qemu::stopped_cpus`] calls halted.
fn the_others_halt_first(mut qemu: QemuInstance, arch: toyos_build::arch::Arch) -> Result<(), String> {
    let cpus = STOP_CPUS as usize;
    let mut console = String::new();
    await_guest(&mut qemu, &mut console, "the fatal path's line past the stop", |c| {
        fatal_past_the_stop(c).is_some()
    })?;
    let fatal = fatal_past_the_stop(&console).expect("awaited above");
    let before = &console[..console.find(FATAL_HALT_NONCE).expect("awaited above")];
    // Non-vacuity: another CPU was making records up to the fatal one.
    if !before.lines().any(|l| l.contains(SIBLING_RECORD) && record_cpu(l).is_some_and(|cpu| cpu != fatal)) {
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
const SIBLING_RECORD: &str = "logstorm t=0 i=0 ";

/// The CPU a kernel record is stamped with.
fn record_cpu(line: &str) -> Option<u32> {
    toyos_logstream::parse(line).filter(|p| p.source == toyos_logstream::Source::Kernel)?.cpu
}

/// The CPU that went fatal, once the console carries its line past
/// `stop_other_cpus`: `panic_reboot::arm`'s, as a machine with a reset whose
/// panic path reads no key says it.
fn fatal_past_the_stop(console: &str) -> Option<u32> {
    let armed = format!("panic: rebooting in {} s, timed by", toyos_tco::PANIC_BOUND_MS / 1_000);
    let lines: Vec<&str> = console.lines().collect();
    let nonce = lines.iter().position(|l| l.contains(FATAL_HALT_NONCE))?;
    let fatal = record_cpu(lines[nonce])?;
    lines[nonce..].iter().any(|l| record_cpu(l) == Some(fatal) && l.contains(&armed)).then_some(fatal)
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
    /// Every outcome's `elapsed`, summed.
    tested: Duration,
}

impl Tally {
    fn new() -> Self {
        Tally {
            passed: 0,
            failures: Vec::new(),
            stalls: Vec::new(),
            invalid: Vec::new(),
            tested: Duration::ZERO,
        }
    }

    fn record(&mut self, outcome: Outcome) {
        self.tested += outcome.elapsed;
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
                 ceilings paid at {:.2}x",
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

        // Summed over the workers: the two add up to the suite's time only
        // in a run one wide.
        let elapsed = format!(
            "{elapsed:.1?}; workers: {:.0?} building, {:.0?} testing",
            toyos_build::build::built(),
            self.tested
        );
        match self.exit_code() {
            1 => say(format!(
                "test result: FAILED. {} passed, {} failed, {} invalidated, \
                 {total} total ({elapsed})",
                self.passed,
                self.failures.len(),
                self.invalid.len(),
            )),
            2 => {
                say(format!(
                    "test result: INVALID. {} passed, {} invalidated by a \
                     host suspend of {suspended:.0?}, {total} total ({elapsed})",
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
                "test result: ok. {} passed, {total} total ({elapsed})",
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
    toyos_build::build::building_for(name);
    eprintln!("{}", toyos_build::printer::started("RUN", name));
    let start = common::clock::mark();
    let outcome = catching(|| match task {
        Task::Machine(name) => run_machine_test(name, test_config),
        Task::Screen(name, profile) => run_screen_test(name, profile, test_config),
    });
    let outcome = Outcome {
        name: name.to_string(),
        reason: outcome.err(),
        elapsed: start.elapsed(),
        suspended: start.suspended(),
    };
    // Here and not where the outcomes are collected: this worker's next task
    // says what it builds, and this line comes before that one.
    report_line(&outcome);
    let _ = report.send(outcome);
}

impl Task {
    /// The test this task reports an outcome for.
    fn name(&self) -> &'static str {
        match self {
            Task::Machine(name) | Task::Screen(name, _) => name,
        }
    }
}

/// One outcome, as the run prints it.
fn report_line(outcome: &Outcome) {
    let reason = || outcome.reason.as_deref().unwrap_or("check failed");
    let line = |word| toyos_build::printer::outcome(word, &outcome.name, outcome.elapsed);
    match outcome.verdict() {
        Verdict::Pass => eprintln!("{}", line("PASS")),
        Verdict::Fail => {
            eprintln!("FAIL {}: {}", outcome.name, reason());
            eprintln!("{}", line(if outcome.stalled() { "STALL" } else { "FAIL" }));
        }
        Verdict::Invalid => eprintln!(
            "{} — the host was suspended for {:.0?} while it ran",
            line("INVL"),
            outcome.suspended
        ),
    }
}

/// Run `tasks` on `width` workers, each printing its outcomes as they land,
/// and return once every worker has joined.
fn run_tasks(tasks: Vec<Task>, width: usize, test_config: &Path) -> Vec<Outcome> {
    if tasks.is_empty() {
        return Vec::new();
    }
    let width = width.clamp(1, tasks.len());
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
        all.extend(rx);
    });
    all
}

/// Every test with a boot, as the task it runs as.
fn build_tasks(
    machine_to_run: &[&'static str],
    screen_to_run: &[(&'static str, qemu::Profile)],
) -> Vec<Task> {
    let machine = machine_to_run.iter().map(|&name| Task::Machine(name));
    let screen = screen_to_run.iter().map(|&(name, profile)| Task::Screen(name, profile));
    machine.chain(screen).collect()
}

/// Whether a run takes `name`: it has no filter, or one that matches it.
fn kept(filters: &[&str], name: &str) -> bool {
    filters.is_empty() || filters.iter().any(|f| name.contains(f))
}

/// The machine tests and the screen tests a run boots.
type Selection = (Vec<&'static str>, Vec<(&'static str, qemu::Profile)>);

/// Every declared test a run [`kept`].
fn select(filters: &[&str]) -> Selection {
    (
        MACHINE_TESTS.iter().filter(|n| kept(filters, n)).copied().collect(),
        SCREEN_TESTS.iter().filter(|(n, _)| kept(filters, n)).copied().collect(),
    )
}

/// Every claim the metal table makes about itself, before anything boots:
/// every row is its test's one declaration and asks for at least one boot, and
/// every boot it asks for is a committed config.
fn check_registration() {
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
    const RUNS: metal::Metal = metal::Metal { arms: METALCASE, judge };
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
    MACHINE_TESTS.iter().copied().chain(SCREEN_TESTS.iter().map(|(n, _)| *n))
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
    let filters = parsed.filters.as_slice();

    let debug_mode = SUITE.present(&args, &testargs::DEBUG);
    let list_mode = SUITE.present(&args, &testargs::LIST);
    let nocapture = SUITE.present(&args, &testargs::NOCAPTURE);

    // How many guests run at once.
    let width = SUITE
        .value(&args, &testargs::JOBS)
        .or_else(|| SUITE.value(&args, &testargs::JOBS_SHORT))
        .map_or(DEFAULT_WIDTH, |n| {
            let width: usize = n.parse().unwrap_or_else(|_| panic!("--jobs: {n:?} is not a width"));
            assert!(width >= 1, "--jobs needs at least one worker");
            width
        });

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

    if let Some(mode) = parsed.metal {
        let (c_bins, rust_bins) = build_shared_bins();
        let mut boots = shared_metal();
        boots.push(c_corpus_metal(&c_bins));
        let (selected, boots) = match metal::select(filters, &parsed.boots, METAL, &boots) {
            Ok(selection) => selection,
            Err(refusal) => {
                eprintln!("[toyos] {refusal}");
                run.exit(1);
            }
        };

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

    if list_mode {
        for name in &registered {
            println!("{name}");
        }
        return;
    }

    if debug_mode {
        let (c_bins, rust_bins) = build_shared_bins();
        run_debug_mode(&c_bins, &rust_bins);
        return;
    }

    let (machine_to_run, screen_to_run) = select(filters);

    // A filter that takes nothing would be dropped in silence beside one that
    // takes something.
    if let Some(dead) = filters.iter().find(|f| !declared().any(|name| name.contains(**f))) {
        eprintln!("No test matches filter {dead:?}");
        run.exit(1);
    }

    let test_config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testcases");
    let mut tally = Tally::new();

    let suite_start = common::clock::mark();

    let tasks = build_tasks(&machine_to_run, &screen_to_run);
    let total = tasks.len();
    eprintln!("\nrunning {total} tests, {width} wide\n");
    run_tasks(tasks, width, &test_config).into_iter().for_each(|o| tally.record(o));

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

    let summary = tally.summary(total, suite_start.elapsed(), suite_start.suspended());
    summary.lines().for_each(|line| eprintln!("{line}"));
    run.exit(tally.exit_code());
}
