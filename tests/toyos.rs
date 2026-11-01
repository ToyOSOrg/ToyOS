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
    self, await_guest, await_marker, await_marker_new, BootOptions, QemuInstance,
    STALLED, TIMED_OUT,
};
use common::{audio, claims, compile, devices, faults, isa, metal, power, ps2server, screen, serial};
use toyos_build::bootlog::{self};
use toyos_metal::testargs::{self, SUITE};

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
    // Each reads action 16 rather than `SYS_SYSINFO`: a verdict about what
    // a process gave back cannot be the whole machine's free memory, which
    // every other binary in a shared boot moves under it.
    "handle_lifetime",
    "shm_release_reclaims",
    "spawn_returns_the_census",
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
    // It writes the log disk for ever, so that its boot's deadline resets the
    // machine under it: `the_disk_survives_a_reset_under_load` runs it alone.
    "stick_load",
    // The log-write load `mask_windows` reads windows under, on the boot
    // whose kernel reports them.
    "log_write",
    // It opens a window, which needs a compositor the shared boot does not
    // run: `a_window_is_drawn_at_the_scale_and_relaid_at_the_next` runs it on
    // tests/images/scalecase.toml, and the `gop_edid_scale` metal row on
    // tests/images/metalcase.toml.
    "scale_window",
    // **Its exit code is a measurement, not a verdict**, and the shared block
    // judges every member on `exit=0` alone — so it would red on every boot
    // that measured anything. It also needs the real-time band, which only
    // `tests/images/latencycase.toml` endows. `latency_wake` runs it there.
    "cyclictest",
    // Its verdict is a ratio of cycle counts, which a guest's host moves: the
    // `wake_storm_cost` metal row runs it on the T14.
    "wake_storm_cost",
    // Its exit code is a round trip's p99 in microseconds, which a guest's host
    // sets: the `wake_onto_busy` metal row runs it on the T14.
    "wake_onto_busy",
    // Its verdict is how the CPU time of a saturated machine split between
    // jobs, which a guest's host sets: the `job_fairness` metal row runs it on
    // each bench.
    "job_fairness",
    // Its product is a cycle count per syscall and the clock rate beside it,
    // which a guest's host sets: the `syscall_cost` metal row runs it.
    "syscall_cost",
    // Its subject is AArch64's `PSTATE.DIT`, of which x86-64 has nothing:
    // `virt_smp` builds it for AArch64 and runs it on that architecture's SMP
    // case.
    "timing_protection",
    // Its product is how long a spawn of `toybox` takes to its exit, which a
    // guest's host sets: the `spawn_cost` metal row runs it.
    "spawn_cost",
    // Its verdict is a duration: whether a shootdown waits for every other CPU
    // is read off the clock around the syscall. The `tlb_shootdown_waits`
    // metal row runs it on the T14.
    "tlb_shootdown_waits",
    // Each reads a global translation its kernel may leave stale, which a TCG
    // guest's TLB drops on any `CR3` write and the T14's keeps: the
    // `a_global_kernel_leafs_discharge_drops_it`,
    // `a_kernel_half_shootdown_drops_a_global_kernel_translation` and
    // `a_user_half_shootdown_keeps_a_global_kernel_translation` metal rows run them.
    "global_leaf_discharge",
    "global_leaf_flush",
    "global_leaf_user_flush",
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
    // Each drives the machine's own i8042 through its `isa` claim: the
    // `isa_ports_are_the_binders_alone` and `isa_lines_reach_their_holder`
    // metal rows run them.
    "isa_grant",
    "isa_lines",
    // It needs a kernel that declared a port of the i8042's row, which only
    // the `isa-port-declared` actuator makes: the
    // `isa_claim_refused_on_a_kernel_port` metal row runs it.
    "isa_claim_refused",
    // It claims the T14's I219, which no guest has: the
    // `claim_reuses_its_remapping_entry` metal row runs it.
    "pci_reclaim",
    // It measures the T14's I219, which no guest has: the
    // `i219_messages_on_its_msi_vector` metal row runs it.
    "i219_msi",
    // It waits for a lease from the bench's router over the T14's I219, which
    // follows the card's bring-up: the `i219_carries_netstacks_link` and
    // `i219_leaves_ulp_unarmed` metal rows run it on tests/images/benchcase.toml.
    "lan_lease",
    // Its product is the T14's counters across an idle span and a spin on
    // every CPU, which the `counters` metal row judges; on a guest it would be
    // seconds of four CPUs spinning, read by nothing.
    "counters_metal",
    // Its product is each CPU's deep idle periods once the ACPI server has
    // declared the firmware's idle states. A guest's idle is its host's: the
    // `idle_entered` metal row judges it.
    "idle_entered",
    // Its product is what a diary record costs, which the `trace_record_cost`
    // metal row reads off the test kernel its flood needs.
    "trace_flood",
    // It takes the machine down; `virt_fatal_halts_the_others_first` runs it.
    "panic_halts_first",
    // They claim QEMU's `edu` and `e1000e`, which only `virt_claim_lpi`'s
    // machine has.
    "claim_lpi",
    "claim_unrouted",
    "claim_bar_fault",
    // Needs a launcher and a declared `cat` and shell, which `tests/images/testcases.toml`
    // does not give: the `process_tree` metal row runs it on tests/images/proctreecase.toml.
    "process_tree",
    // Needs a launcher and a declared `toybox` holding `roster`, which
    // `tests/images/testcases.toml` does not give: the `launch_toctou` metal row runs it on
    // tests/images/proctreecase.toml.
    "launch_toctou",
    // A kernel primitive with no use for any one boot's devices: the
    // `port_badge` metal row runs it on tests/images/proctreecase.toml.
    "port_badge",
    // Needs a launcher whose row lists `/apps`, which `tests/images/testcases.toml` does
    // not give: the `app_view` metal row runs it on tests/images/proctreecase.toml.
    "app_view",
    // It holds every frame a process may, and every other process's
    // allocation is refused meanwhile, so whatever shared its boot could die
    // of it: `kernel_objects_refused_alone` runs it on QEMU and the
    // `kernel_objects_refused` metal row on the T14.
    "kernel_objects_refused",
    // It holds every frame an ordinary job may, and needs the floors of
    // `tests/images/floorcase.toml`: `memory_floor_holds_the_controls` runs it
    // on QEMU and the `memory_floor` metal row on the T14.
    "memory_floor",
    // Fills /tmp to the VFS listing limit, so it needs a boot nothing else
    // shares — every later `read_dir("/tmp")` in it would be refused.
    // `readdir_bound` gives it one.
    "readdir_bound",
    // They need an HDA controller, which the shared boot's machine has not:
    // the `hda_tone`, `hda_tone_resampled` and `hda_volume_step` metal rows
    // run them on the T14 and the AMD bench, and
    // `hda_plays_to_the_controller_that_reaches_an_output` runs `volume_step`
    // on QEMU's.
    "audio_tone",
    "audio_tone_resampled",
    "tone_beside_log_write",
    "volume_step",
    // It hands the ACPI claim to a server of its own and kills it, which needs
    // a boot that starts none: the `acpi_server_death` metal row runs it on
    // tests/images/acpicase.toml.
    "acpi_release",
    // It waits for the ACPI server's power sources, and no guest's machine
    // has a battery: the `acpi_tables_loaded`, `acpi_server_events` and
    // `battery_read` metal rows end their boot on it.
    "acpi_hold",
    // It waits for ps2server's probe to end, which only a boot that starts
    // ps2server has: every `metalcase` row ends its boot on it.
    "ps2server_hold",
    // It waits for the ACPI server's count of its embedded controller's
    // queries, and no guest's machine has that controller: the
    // `acpi_server_events` metal row runs it.
    "acpi_counted",
    // It waits for tgl-display's DC6 allow, which only the T14's iGPU has:
    // the `display_output_presents` metal row runs it.
    "tone_after_dc6",
    // It claims the fixed hardware itself, which needs a boot that starts no
    // server, and stages the firmware's side of the Global Lock, which needs
    // the test kernel: `acpi_mediated_access` and `acpi_press_is_one_sci`
    // run it on tests/images/acpicase.toml.
    "acpi_mediated",
    // It powers the machine off: `machine_shutdown_short_stop` runs it.
    "stop_short",
    // It claims QEMU's virtio NIC, which the T14 has none of: `bar_map_again`
    // runs it.
    "bar_map_again",
    // It claims the virtio NIC and asks how its BAR is lent, and the T14 has
    // no virtio NIC: `lent_device_memory_refused` runs it.
    "lent_device_memory_refused",
    // It claims the radios no program holds, which a shared boot must not
    // take from its members: `bar_map_again` and the
    // `msix_table_read_in_place` metal row run it.
    "msix_in_place",
    // It needs ToyOS's own clang and LLD packaged on ROOT, which only its boot
    // stages: `hosted_clang_hello` runs it on tests/images/hostedclangcase.toml.
    "hosted_clang_hello",
    // A std child's stdio, beside libc's: `libc_children` runs it on
    // tests/images/libccase.toml.
    "stdio_named",
    // libc's `recv` across a `close` of its descriptor, as its own netstack:
    // `libc_children` runs it on tests/images/libccase.toml.
    "libc_recv_beside_close",
    // A quit's walk over a tree of its own children: `libc_children` runs it
    // beside the children libc starts, on tests/images/libccase.toml.
    "quit_tree",
    // libc's `read` across a handler, ordered by the roster: `libc_children`
    // runs it on tests/images/libccase.toml.
    "libc_read_restarts",
    // A shell's Ctrl+C, the probe being its surface: `libc_children` runs it
    // on tests/images/libccase.toml, whose shell and `cat` it drives.
    "tty_interrupt",
    // A stream, a listener and a datagram socket exported to a child through
    // real netstack: they reach the harness, which only
    // `netstack_holders_exported_to_children`'s boot sets up.
    "stream_export",
    "socket_export",
    // A stream's holders across processes, as their own netstack: it spawns a
    // child that faults, which a shared boot's other members must not watch,
    // and `stream_holders` runs it.
    "stream_handles",
    // A datagram receive bounded by its read timeout, through real netstack,
    // which a shared boot runs none of: `udp_receive_times_out` runs it.
    "udp_recv_timeout",
    // It claims the NIC and starts its driver itself, which only
    // tests/images/linkcase.toml leaves it to:
    // `a_link_ends_under_a_held_stream_and_comes_back` runs it.
    "link_restart",
    // A connect killed while netstack has not answered it, through real
    // netstack, which a shared boot runs none of:
    // `a_client_killed_mid_connect_gives_its_place_back` runs it.
    "netstack_connect_killed",
    // It needs two NICs and a harness that sets one's link down:
    // `traffic_leaves_by_the_second_link_while_the_first_is_down` runs it.
    "link_failover",
    // It needs a launcher: `a_launch_gives_each_unsent_stdio_slot_a_null`
    // runs it on tests/images/launchcase.toml.
    "launch_unsent_stdio",
    // It needs a `login` connector and the session rows: `session_lifecycle`
    // runs it on tests/images/sessioncase.toml.
    "session_lifecycle",
    // It claims the xHCI controller no image's usbd row names, which only a
    // machine with a spare has: `usbd_drives_the_spare` and the
    // `usbd_drives_the_type_c_controller` metal row run it.
    "usbd_spare",
    // `usbd_spare`'s fault and foreign-vector arms, one binary each because a
    // metal job takes no argument: the AMD bench's
    // `amdvi_usbd_drives_its_own_entry` row runs them.
    "usbd_spare_fault",
    "usbd_spare_vector",
    // The same controller, handed to a holder that is killed and claimed
    // again at once, in the same boots as `usbd_spare`.
    "end_returns_claims",
    // It reads two `edu` functions each held by a `drives` row's process,
    // which only `a_drives_row_holds_one_process_per_function`'s boot has.
    "drives_judged",
    // Its product is a table of timings beside ring, which a guest's host
    // sets: the `crypto_instrument` metal row runs it on the T14 and the AMD
    // bench.
    "crypto_parity",
    // Their product is timing evidence on a bench's own CPU, a million
    // measurements per routine: the `crypto_secret_timing` and
    // `crypto_secret_timing_curves` metal rows run them on the T14 and the
    // AMD bench.
    "crypto_constant_time",
    "crypto_constant_time_curves",
];

/// The shared boot's last members, in this order: each fills a bound of its
/// own process — thousands of threads, a thousand library images — and its
/// exit gives all of it back. Behind every
/// discovered member and every corpus case, so nothing but the reboot runs
/// after one of these. **This is their one declaration**:
/// [`discover_rust_tests`] leaves them to it, so a name taken off this list is
/// a discovered member again.
const LAST_MEMBERS: &[&str] = &["abuse_thread_table", "abuse_dlopen_ledger"];

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
    // Its shared run is each bench's verdict;
    // `std_threads_count_the_cpus_and_give_back_ended_threads` runs it under QEMU.
    "std_threads",
    // Its shared run is each bench's verdict, on the T14's AVX-512 and the AMD
    // bench's AVX; `vector_state_survives_a_switch` runs it under QEMU.
    "vector_state",
    "std_alloc",
    // Its shared run asserts what holds of one boot's draws on x86-64;
    // `virt_random_differs` builds it for AArch64, runs it on that
    // architecture's job case, and compares two boots' draws.
    "random_draws",
    // Its shared run is the T14's verdict, on its served root;
    // `a_spawn_from_an_image_frees_it_when_the_child_ends` runs it under QEMU.
    "spawn_returns_the_census",
    // Its shared run is ring's known answers in a guest; the crypto boot runs it
    // first on each bench, and `crypto_instrument_prints_its_table` before the
    // instrument under QEMU.
    "ring_kat",
    // Its shared run asserts what holds of `SYS_CLOCK_EPOCH` on x86-64;
    // `virt_wall_clock_utc` builds it for AArch64 and judges what it read
    // against the instant the job case stages in the RTC.
    "wall_clock_now",
];

/// What the boot's audit of the kernel's tables says when every leaf passed
/// (`kernel/src/mm/mod.rs`).
const KERNEL_WX_HOLDS: &str = " kernel leaves, none writable and executable, none executable outside the text ";

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
    // The same fatal path from inside a boot checkpoint's painter, holding the
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
    // `kernel_wx_leaf_is_refused` on `virt`: no metal bench is AArch64.
    ("virt_kernel_wx_leaf_is_refused", qemu::Profile::Virt),
    ("virt_el2_drop", qemu::Profile::VirtEl2NoVhe),
    ("virt_user_mode", qemu::Profile::Virt),
    ("virt_timer_preempts", qemu::Profile::Virt),
    ("virt_irq_storm", qemu::Profile::Virt),
    ("virt_timer_floor", qemu::Profile::Virt),
    ("virt_fp_isolation", qemu::Profile::Virt),
    ("virt_first_entry", qemu::Profile::Virt),
    ("virt_unmap_touch", qemu::Profile::Virt),
    ("virt_debug_refused", qemu::Profile::Virt),
    ("virt_readonly_copyout", qemu::Profile::Virt),
    ("virt_ring0_timer_in_syscall", qemu::Profile::Virt),
    // Emulated, with `virt_el1_smp`, `virt_off_names_the_cpus_left_on` and the
    // reboots: under HVF a `klogd` the stop wakes can go undispatched past the
    // stop's whole wait for the console's wire
    // (issues/a-woken-klogd-can-wait-seconds-on-an-idle-cpu-under-hvf.md).
    ("virt_mask_windows", qemu::Profile::VirtEl2),
    ("virt_smp", qemu::Profile::VirtEl2),
    ("virt_el1_smp", qemu::Profile::VirtTcg),
    ("virt_failed_ap_leaves_no_hole", qemu::Profile::Virt),
    ("virt_fatal_halts_the_others_first", qemu::Profile::Virt),
    // Emulated at EL2: its last word comes through the same stop, and it is
    // the one test of `SYSTEM_RESET` through the SMC conduit.
    ("virt_reboot", qemu::Profile::VirtEl2),
    ("virt_off_names_the_cpus_left_on", qemu::Profile::VirtEl2),
    ("virt_reboot_refused_without_psci", qemu::Profile::Virt),
    ("virt_reboot_wire_held", qemu::Profile::VirtEl2),
    ("virt_reboot_wire_kept", qemu::Profile::VirtEl2),
    // The job case entered at EL2, once: the entry's EL2 writes for the timer
    // and FP, which no boot under HVF runs.
    ("virt_jobs_at_el2", qemu::Profile::VirtEl2),
    ("virt_random_differs", qemu::Profile::Virt),
    ("virt_smmu", qemu::Profile::VirtSmmu),
    ("virt_claim_lpi", qemu::Profile::VirtIts),
    ("virt_no_seed_refused", qemu::Profile::VirtNoRng),
    ("virt_wall_clock_utc", qemu::Profile::Virt),
    ("virt_low_ecam", qemu::Profile::VirtLowEcam),
];

/// Which handle the loader took the panel's EDID from, and what the compositor
/// made of it, on each bench: the handle M1 found the protocol on, and the
/// size and scale `lib/edid`'s fixture of that bench's panel gives; the
/// desktop drawn at that scale, and `scale_window`'s 200x100 logical window
/// given its buffer at it.
fn gop_edid_scale(loader: &str, log: &str, kernel: &str) -> Result<(), String> {
    let signature = microcode_lines(kernel)?
        .first()
        .ok_or("the kernel wrote no microcode line to tell the bench by")?
        .signature;
    let (handle, panel) = match signature {
        T14_SIGNATURE => (0, "panel 309x173 mm, scale 1.5"),
        AMD_BENCH_SIGNATURE => (1, "panel 309x174 mm, scale 1.5"),
        other => return Err(format!("no bench of signature {other:#x} says which panel it has")),
    };
    let handed = format!("EDID: GOP handle {handle} of 2 shows the scanout and publishes 128 byte(s), the base block handed over");
    if !loader.contains(&handed) {
        let said: Vec<&str> = loader.lines().filter(|l| l.contains("EDID: ")).collect();
        return Err(format!("loader.log does not say {handed:?}; its EDID lines: {said:?}"));
    }
    let chosen = format!("compositor: display 1920x1080, {panel}");
    if !log.contains(&chosen) {
        let said: Vec<&str> = log.lines().filter(|l| l.contains("compositor: display ")).collect();
        return Err(format!("the log does not say {chosen:?}; the compositor said {said:?}"));
    }
    if let Some(kept) = log.lines().find(|l| l.contains("compositor: drawing at scale ") && l.contains(", kept ")) {
        return Err(format!(
            "the bench keeps a scale of its own in /state/compositor/scale, so the panel's is not what it draws at: \
             {kept:?}; `screen scale auto` on it gives the panel's back"
        ));
    }
    for drawn in ["compositor: drawing at scale 1.5", "scale_window: logical 200x100 drawn 300x150 at scale 1.5"] {
        if !log.contains(drawn) {
            let said: Vec<&str> =
                log.lines().filter(|l| l.contains("compositor: ") || l.contains("scale_window: ")).collect();
            return Err(format!("the log does not say {drawn:?}; the compositor and the window said {said:?}"));
        }
    }
    eprintln!("  [gop_edid_scale] signature {signature:#x}: GOP handle {handle}, {panel}, a 200x100 window drawn 300x150");
    Ok(())
}

/// The tests whose machine shape *is* the test, each on a boot of its own.
/// `run_machine_test` dispatches them.
const MACHINE_TESTS: &[&str] = &[
    // A `drives` row's one process per function, on QEMU x86-64's q35 with
    // two `edu` (`Profile::HeadlessTwoEdu`): which functions get one is
    // `toyos-supervisor`'s, host-tested, and this is the supervisor
    // performing it — two claims by arrival on identical functions, two
    // spawns, each process holding its own — which no host build has. No
    // bench has two identical functions the kernel leaves to a claim.
    "a_drives_row_holds_one_process_per_function",
    // Two HDA controllers, each with an `hda` driver behind VT-d: the one
    // with no codec plays nothing, soundserver plays to the other, and the
    // master job the benches' rows judge steps that output's codec. Which
    // driver offers and which soundserver plays to are three processes and
    // two claims, which no host build has. The tone rows' stream is the
    // benches' alone. q35 only: `virt` has no unit to put the functions
    // behind.
    "hda_plays_to_the_controller_that_reaches_an_output",
    // Whether QEMU's virtio functions negotiated `VIRTIO_F_ACCESS_PLATFORM`
    // behind its emulated VT-d unit and without one: no shipped machine has a
    // virtio function, so only a QEMU machine can be asked.
    "iommu_virtio_platform",
    // A virtio NIC created without `iommu_platform=on` on q35 with VT-d, so
    // its DMA bypasses the unit: the supervisor refuses it to netstack by
    // name. Only QEMU can create such a function; q35 only.
    "a_virtio_function_without_access_platform_is_refused",
    // The VM's console port unplugged under a running guest: its driver
    // ends, logkeeper sees its carrier gone and the UART carries the log
    // again. Three processes, the kernel and QEMU's hot-unplug, which no host
    // build has. q35 only: no other machine carries a console port.
    "the_uart_carries_the_log_again_once_the_console_carrier_goes",
    // An AMD-Vi unit left blocking every device, switched off and programmed
    // before the boot disk is driven behind it: the register sequence and the
    // DMA it lets through are no host test's, and the AMD bench is staged by
    // hand rather than by CI.
    "amdvi_firmware_left",
    // A VT-d unit left translating, with no protected memory regions to guard
    // its hand-over: the T14's units have them, so only QEMU's takes this one.
    "vtd_firmware_left",
    // A translating AMD-Vi unit's refusal, recorded and read: a kernel-owned
    // record halts the machine, so no metal row reaches it, and only QEMU's
    // `iommu-testdev` aims a DMA where no domain maps.
    "amdvi_selftest",
    // The nested-NMI report is a raw write to the 16550, which the T14 does not
    // have.
    "nested_nmi_is_loud",
    // The boot's W^X audit refusing a leaf an actuator staged: the panic ends
    // the boot, so it cannot share one, and the T14's `kernel_wx` row reads
    // the same audit passing. q35 on `Profile::Metal`, whose 16550 carries
    // the panic.
    "kernel_wx_leaf_is_refused",
    // The clock's response to TSC steps an actuator stages where
    // `IA32_TSC_ADJUST` would not show them: the first slewed away, the
    // second demoting the TSC, after which processes' readings of now reach
    // the kernel. q35 on `Profile::Metal`; the order, the step's size and a
    // demoted reading are what it holds, and no bench's TSC steps on demand.
    "a_stepped_tsc_is_slewed_then_demoted",
    // Its counterpart: the HPET an actuator stalled while the TSC and the PM
    // timer count on is left out, and the boot goes on. q35 on
    // `Profile::Metal`, whose PM timer is at 0x608; no bench's HPET stalls on
    // demand.
    "a_stalled_hpet_is_left_out",
    // A press of the fixed power button reaches the `acpi` claim's holder as
    // one SCI, its line masked until the holder serves it: the kernel's SCI
    // and its mask have no host build, QEMU raises the event on demand, and
    // nothing presses the T14's button but a hand.
    "acpi_press_is_one_sci",
    // The osbench groups that use the runner's capability for a row of their
    // own, on the image they are measured on, on QEMU x86-64's q35: which
    // endowments a job gets is the image's and the supervisor's, which no
    // host build has, and the benches run osbench only nightly.
    "osbench_capability_groups_exit_0",
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
    // The power-off with `klogd` holding the console's wire as the stop
    // begins, on one CPU: a staged hold of a kernel thread, read off a console
    // QEMU keeps past the power-off, and the T14 is never asked to power off.
    "machine_shutdown_wire_held",
    // The same with `klogd` keeping the wire through the stop's budget.
    "machine_shutdown_wire_kept",
    // The same with `klogd` staged inside its hold of the wire from the boot's
    // last word past the seal, where the stop does not hold it: the one CPU
    // runs nothing but the stop from the seal on.
    "machine_shutdown_wire_at_the_seal",
    // A claimable function with a mappable BAR that no program of the boot
    // holds: QEMU's virtio NIC on `tests/images/testcases.toml`. The T14's one such
    // function is its I219, and a kernel that dies on this takes the bench's
    // machine down with it.
    "bar_map_again",
    // The same claim with the NIC behind a root port: the BAR is handed over
    // where firmware put it inside the port's window, which no host test
    // reads off firmware's enumeration. Behind a bridge on metal it is the
    // AMD bench's `amdvi_usbd_drives_its_own_entry`.
    "bar_map_behind_a_bridge",
    // A region that is not memory the kernel allocated, lent as a window: the
    // kernel describes the BAR of a function no program of the boot holds,
    // which on the T14 is its I219, as `bar_map_again` says.
    "lent_device_memory_refused",
    // `images/console.toml`'s image, which runs no job and hands no machine
    // back: a metal boot of it ends with a hand on the power button.
    "console_image_boots",
    // The UEFI loader's check of a boot program, which no host test reaches:
    // a slot whose supervisor is bent is refused before boot services end,
    // and the other slot boots. On both architectures' QEMU machines; the
    // `bent_supervisor_falls_back` metal row runs it on a bench.
    "a_bent_supervisor_is_refused_and_the_other_slot_boots",
    // A spawn and its child's end leave the census as it was, for a program
    // the root server holds in memory, one on DATA read whole and one in the
    // kernel's `/tmp`: the image's
    // owners and the kernel's object lifetimes together, which no host test
    // holds. One boot of QEMU's x86-64 machine on the actuator kernel, for the
    // census.
    "a_spawn_from_an_image_frees_it_when_the_child_ends",
    // The boot's log written through usbd onto the stick it booted from, and
    // still there once the machine has stopped: the path every metal boot's
    // `/log` takes, which no other QEMU row reads, because their console is
    // the serial line. Its oracle is the image read back on the host.
    "the_log_lands_on_the_boot_stick",
    // Keys QEMU's `usb-kbd` reports, through usbd, to the owner of input:
    // the console image's shell runs the command they spell. QEMU's xHC and
    // keyboard are the independent end of the report the stub cannot be.
    "a_usb_key_reaches_the_console",
    // The desktop on CPUs that offer no x2APIC, whose local APICs run in
    // xAPIC mode: every IPI, EOI and timer write goes through the MMIO page,
    // which no host build has and the T14, whose CPUs offer x2APIC, never
    // reaches; the AMD laptop that needs it is not on the bench.
    "desktop_without_x2apic",
    // A machine whose one NVMe disk is all it has boots the image off it and
    // serves every role off it: the firmware that reads it, the loader and
    // kernel it hands the image to, and the claim the nvme driver serves the
    // block service's sessions through have no host build. A bench boots the
    // same way from its own disk, beside another system's partitions.
    "nvme_disk_boots",
    // A stick boot beside an NVMe disk that is another system's, on a
    // controller under Intel's id: the row drives it by its class through the
    // kernel's claim, the block service and the file servers, none of which a
    // host test runs together, and the disk is read back whole on the host
    // after the stop. QEMU x86-64; on a bench it is `nvme_leaves_the_benchs_disk_alone`.
    "a_host_disk_beside_the_stick_is_left_alone",
    // The same disk's DATA designated from inside the guest: the supervisor
    // minting the designation image's one authorization, the block service
    // resolving and routing it, and the NVMe driver judging, stamping,
    // flushing and reading back are four processes and a claim no host build
    // runs together; the host reads the disk image after the stop for the
    // stamp and for every other byte. QEMU x86-64; on a bench it is
    // `bench_data_designated`.
    "the_data_partition_beside_the_stick_is_designated",
    // clang and LLD built for ToyOS, installed by `pkg` and compiling and
    // linking inside it: the claim is that they install and run on ToyOS,
    // which no host test can make, and the T14 would read nothing a guest does
    // not. On demand: [`ON_DEMAND`]. Red at the link until item 5 of
    // `issues/toyos-builds-itself.md` lands.
    "hosted_clang_hello",
    // libc's children, and a quit's walk over a tree of std children: each
    // program starts and waits for its own, which is order, completion,
    // contents and counts, and no hardware is in it.
    "libc_children",
    // A stream shared by a parent and the child it was exported to, the test
    // its own netstack: when a dead holder's handles close, and that the
    // survivor's write and read then work, are the kernel's, which no host
    // build has; no hardware is in it. x86-64 alone, the architecture the
    // suite builds guest binaries for (`qemu::SUITE_ARCH`).
    "stream_holders",
    // A stream, a listener and a datagram socket exported through std's
    // `Command` and adopted in a child, through the real netstack of
    // `tests/images/netcase.toml`: the export, the endowment, the adoption and
    // a second process's accept and datagrams on one queue cross real
    // processes and the real netstack, which no host test and
    // `stream_holders`' stand-in show. The harness is the peer, on the host's
    // loopback that QEMU's user network reaches and forwards one port of, so
    // no public network. Both jobs in one boot, once on virtio and once on
    // QEMU's `e1000e`, which the image's I219 row matches by its id alone: a
    // NIC no row names a machine for, up with no configuration of its own.
    // x86-64 alone (`qemu::SUITE_ARCH`).
    "netstack_holders_exported_to_children",
    // A datagram receive with a read timeout ends at it, a timeout under a
    // millisecond included, and a zero timeout is refused: std's conversion
    // of every socket timeout and its datagram wait, through the real
    // netstack of `tests/images/netcase.toml`, which no host build has. No
    // peer and no public network. Order and completion: QEMU's clock bounds
    // nothing but the hang. x86-64 alone (`qemu::SUITE_ARCH`).
    "udp_receive_times_out",
    // A stream netstack holds across its link's end: the NIC's driver killed
    // and started again on a fresh claim, and a line answered on the stream
    // both before and after. netstack keeping its node across a link's end,
    // the driver's hello on a new region and the kernel's claim given back
    // and minted again cross three processes and a device, which no host test
    // holds. QEMU x86-64 (`qemu::SUITE_ARCH`); the harness is the peer, on
    // the host's loopback that QEMU's user network reaches, so no public
    // network. Order and completion only.
    "a_link_ends_under_a_held_stream_and_comes_back",
    // A client killed while its connect is unanswered gives its place back to
    // the real netstack of `tests/images/netcase.toml` at once, where only its
    // connection can say it left: a process's death and netstack's watch of
    // it, which no host test has. The peer is an address on QEMU's user
    // network nothing answers, so no public network. Order and counts, QEMU's
    // clock a hang ceiling only: the kill has to land within the about 3 s
    // netstack takes to give the address up, or the test is red. x86-64 alone
    // (`qemu::SUITE_ARCH`).
    "a_client_killed_mid_connect_gives_its_place_back",
    // netstack over two links: the default route leaves by the first, by the
    // second while QMP holds the first's NIC down, and by the first again
    // when it is up. Two drivers' links, a NIC's carrier and netstack's
    // routes cross three processes and two devices, which the node's and the
    // shard's host tests of rank, failover and return hold below the drivers.
    // QEMU x86-64 (`qemu::SUITE_ARCH`), on two user networks; the datagrams go
    // to RFC 5737's TEST-NET-1, which answers nothing, and the harness is the
    // peer on the host's loopback, so it depends on no public network. The networks are not
    // `restrict=on`: with it the job's first datagram off both networks was
    // refused for want of a route, and the default route is what is tested.
    // Order and counts only.
    "traffic_leaves_by_the_second_link_while_the_first_is_down",
    // Login sessions: their owner parents, signs out and ends with each, and
    // the machine's reboot waits for them until forced, which only processes
    // in one boot show, as order, completion and counts. The reboot ends the
    // boot, so it is last and the boot is the test's own.
    "session_lifecycle",
    // A launch's unsent stdio slots: its own boot, because the launcher it
    // needs makes every spawn of a declared program a launch.
    "a_launch_gives_each_unsent_stdio_slot_a_null",
    // A job reaches its row's own `/state/<name>` and nothing else of `/state`:
    // which grants the supervisor mints and the file server honours, which no
    // host build has. Contents and refusals, and no hardware is in it.
    "a_row_reaches_its_own_state_and_no_other",
    // A controller no row names, handed over to a claim whose holder's No-Op
    // is answered by an interrupt through it, and a transfer aimed past the
    // claim's grants: the hand-over, the interrupt through the claim and the
    // unit have no host build, and the T14's unit is not asked to fault. What
    // usbd does on the bus is `toyos-xhci`'s stub tests' to hold.
    "usbd_drives_the_spare",
    // The same behind QEMU's AMD unit, remapping and with no SMI filter: the
    // claim is refused by that reason before anything on the function
    // changes. No metal machine in reach has an AMD-Vi unit without the
    // filter.
    "usbd_refused_without_smi_filter",
    // The same machine with its AMD unit remapping nothing: the claim is
    // refused by that reason. No metal row asks it: no bench is without
    // remapping.
    "usbd_refused_without_amdvi_remapping",
    // Every vector register a thread holds, across switches to threads holding
    // others, against a scheduler, which no host build has. The shared boot's
    // run is the benches'; this is TCG's, whose XSAVE is QEMU's own reading of
    // the SDM, and the KVM runner's CPU.
    "vector_state_survives_a_switch",
    // std's CPU count against the CPUs the guest booted with, and an ended
    // thread's stack and table place given back, joined or detached, on the
    // two-CPU q35 guest: std's ToyOS layer, which no host builds.
    "std_threads_count_the_cpus_and_give_back_ended_threads",
    // A process holding every frame it may is refused every kernel object it
    // asks for more, and the kernel never panics for it: on the two-CPU,
    // 4 GiB q35 guest, in a boot of its own, since every other process is
    // refused meanwhile. QEMU establishes the refusals and that the machine
    // goes on; nothing in it is timing.
    "kernel_objects_refused_alone",
    // With one ordinary job holding every frame it may, the supervisor answers
    // a stop's request, a key reaches the owner of input through ps2server, a
    // system holder kills the greedy job, and a service the exhaustion ended is
    // serving again once the memory is given back: the kernel's accounts and
    // floors, the supervisor's restart and the input path together, which no
    // host model has. On the two-CPU, 4 GiB q35 guest in a boot of its own,
    // `tests/images/floorcase.toml`; QEMU types the key. Order and completion
    // only: nothing in it is timing. The `memory_floor` metal row runs it on the
    // T14 without the key.
    "memory_floor_holds_the_controls",
    // The parity instrument's two builds in one boot, behind ring's known
    // answers: that every row runs and both builds print the same whole table.
    // Its timings decide nothing here; the `crypto_instrument` metal row takes
    // them on the benches.
    "crypto_instrument_prints_its_table",
    // The clipboard agent's opening exchange with QEMU's own vdagent: the
    // device exists only under QEMU, so no metal row reaches it, and the other
    // end of the handshake and of the request is QEMU's vdagent chardev and
    // virtio-serial device, which no host test runs.
    "the_opening_exchange_with_qemus_agent_completes",
    // The compositor driving QEMU's virtio-gpu through its claim, and a host
    // display resize reaching the guest's mode: no metal machine has a
    // virtio-gpu, and the host tests drive `toyos-virtio-gpu` against a model
    // written from this design's own reading of §5.7, so only QEMU's device
    // says it was read right. x86-64 q35 alone, `Profile::HeadlessVirtioGpu`:
    // an AArch64 `virt` boot hands over no PCI claim yet. A boot of its own:
    // its `-vnc` display changes the display configuration the clipboard
    // test's premise, `-display none`, rests on.
    "the_compositor_drives_the_virtio_gpu",
    // A client's window, asked for in logical pixels at scale 2, is drawn at
    // twice that on the panel, and the display port's `screen scale 1` relays
    // it to its logical size with the chrome around it: what reaches the
    // panel is the compositor's buffer swap, its chrome and the window
    // protocol's scale meeting on a running desktop, which no host test
    // composes. On `Profile::Metal`'s q35, whose 1920x1080 GOP panel holds 2
    // and whose firmware publishes no EDID, so the image's `scale=2` sets it;
    // no metal machine's glass can be read.
    "a_window_is_drawn_at_the_scale_and_relaid_at_the_next",
];

/// **The metal profile**: which registrations run on a bench, what boots each
/// one needs, and how each is judged off the log the boot left on the bench's
/// disk.
///
/// Two rows naming the same boot share one image and one boot, and their job
/// lists are unioned. A boot is about a minute of the machine's time, so that
/// grouping is what the suite's cost is; the boot is *named* by an arm rather
/// than derived from its config and parameters, because sharing is not always
/// safe and only the author knows.
const METAL: &[(&str, metal::Metal)] = &[
    (
        "bent_supervisor_falls_back",
        metal::Metal { arms: BENT_SUPERVISOR, judge: |b| bent_supervisor_fell_back(&b[0].loader(), &b[0].kernel()) },
    ),
    // ---- one image: tests/images/testcases.toml, no parameters, one job list ----
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
        "kernel_objects_refused",
        metal::Metal {
            arms: TESTCASES_REFUSED,
            judge: |b| b[0].job_passed("test_rs_kernel_objects_refused"),
        },
    ),
    (
        "memory_floor",
        metal::Metal { arms: FLOORCASE, judge: |b| b[0].job_passed("test_rs_memory_floor") },
    ),
    (
        "readdir_bound",
        metal::Metal {
            arms: TESTCASES_READDIR,
            judge: |b| b[0].job_passed("test_rs_readdir_bound"),
        },
    ),
    (
        "wake_storm_cost",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| b[0].job_passed("test_rs_wake_storm_cost"),
        },
    ),
    // The owner's directive (2026-10-14): no audible sound tests on the bench
    // laptops, so these rows run with soundserver's master muted, which none
    // of their claims needs to hear (`tests/guest/rust-tests/src/master.rs`).
    (
        // A tone through soundserver's HDA path. The T14 and the AMD bench.
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
        // `hda_tone`'s tone while a thread of its process writes the log
        // disk, the bench's NVMe: no period soundserver had to fill. It is the
        // kernel's: one claim's interrupts and its holder's threads never
        // starve another holder's deadline. The T14 and the AMD bench.
        "tone_beside_a_log_write",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed(audio::TONE_BESIDE_LOG_WRITE)?;
                audio::tone_beside_a_log_write(&b[0].log())
            },
        },
    ),
    (
        // A tone at the device rate the device does not run, through cpal,
        // played to its end with every period covered: soundserver resamples it.
        // The T14 (44.1 kHz) and the AMD bench (48 kHz).
        "hda_tone_resampled",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_audio_tone_resampled")?;
                audio::resampled_tone_on_metal(&b[0].log())
            },
        },
    ),
    (
        // One step of the codec's master up from its bottom and back, each
        // read back from the codec, on the HDA path soundserver configured,
        // and the master muted again. The T14 and the AMD bench.
        "hda_volume_step",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_volume_step")?;
                b[0].log().must_say(audio::PLAYING).map(|_| ())
            },
        },
    ),
    // ---- one image: tests/images/testcases.toml armed with the chipset watchdog ----
    (
        // Two boots, and they cannot merge: the control is the same image
        // *without* the parameter, which is the boot every row above rides —
        // so this costs one image and not two, and its control is a machine
        // several other verdicts were already taken from.
        "loader_watchdog_arms",
        metal::Metal {
            arms: &[
                metal::once("testcases-watchdog", "tests/images/testcases.toml", &["watchdog"], &[]),
                // The batch's job list is the union of its riders'; this arm
                // needs none of its own.
                metal::once("testcases", "tests/images/testcases.toml", &[], &[]),
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
        // is held and what is only read.
        "counters",
        metal::Metal {
            arms: &[metal::once("testcases", "tests/images/testcases.toml", &[], &["test_rs_counters_metal"])],
            judge: |b| counters_on_metal(b[0]),
        },
    ),
    (
        // The ACPI server on the machine its stage is for: the row the kernel
        // filled from the T14's own tables, the server armed on it, and the
        // embedded controller's events taken and counted, its job waiting
        // past the server's count interval.
        "acpi_server_events",
        metal::Metal {
            arms: &[metal::Arm { jobs: &["test_rs_acpi_counted"], ..TESTCASES_HELD[0] }],
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
        // The AMD bench's declared idle states as every CPU keeps them, and
        // each CPU in one of them across an idle second:
        // `idle_entered_on_metal` says what is held. The same boot as
        // `acpi_tables_loaded`.
        "idle_entered",
        metal::Metal {
            arms: &[metal::Arm { jobs: &["test_rs_idle_entered"], ..TESTCASES_HELD[0] }],
            judge: |b| idle_entered_on_metal(b[0]),
        },
    ),
    (
        // The T14's battery and AC adapter as the server read them through
        // its AML and its embedded controller: `battery_on_metal` says what
        // is held, and prints the readings.
        // The same boot as `acpi_server_events`, held to the server's power
        // sources, which follow the first reading.
        "battery_read",
        metal::Metal {
            arms: TESTCASES_HELD,
            judge: |b| battery_on_metal(b[0]),
        },
    ),
    (
        // The server killed: the kernel writes `ACPI_DISABLE` as its claim goes,
        // and `SCI_EN` reads clear after it.
        "acpi_server_death",
        metal::Metal {
            arms: &[metal::once("acpicase", "tests/images/acpicase.toml", &[], &["test_rs_acpi_release"])],
            judge: |b| acpi_death_on_metal(b[0]),
        },
    ),
    (
        // The two numbers `syscall_cost` measures, printed by the job and read
        // off the log: that it ran, and what it said, are the verdict.
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
        // How long a spawn of `toybox` takes to its exit, printed by the job and
        // read off the log for an A/B against another build: that it ran, and
        // what it said, are the verdict.
        "spawn_cost",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_spawn_cost")?;
                b[0].log().must_say("spawn_cost: /system/bin/toybox min ")?;
                Ok(())
            },
        },
    ),
    (
        // `SYS_DEBUG` holds each other CPU's shootdown acknowledgement back,
        // so the kernel that carries it; the verdict is the guest's own exit,
        // and every wait it asserts is a floor, which no load shortens.
        "tlb_shootdown_waits",
        metal::Metal {
            arms: &[metal::Arm {
                features: ACTUATOR_KERNEL,
                ..metal::once("shared-debug", "tests/images/testcases.toml", SELFTEST_PARAMS, &["test_rs_tlb_shootdown_waits"])
            }],
            judge: |b| b[0].job_passed("test_rs_tlb_shootdown_waits"),
        },
    ),
    (
        // A global kernel translation outlives its entry until something drops
        // it, and the kernel that carries `SYS_DEBUG` stages one: the verdict
        // is the guest's own exit, its control first.
        "a_global_kernel_leafs_discharge_drops_it",
        metal::Metal {
            arms: &[metal::Arm {
                features: ACTUATOR_KERNEL,
                ..metal::once("shared-debug", "tests/images/testcases.toml", SELFTEST_PARAMS, &["test_rs_global_leaf_discharge"])
            }],
            judge: |b| b[0].job_passed("test_rs_global_leaf_discharge"),
        },
    ),
    (
        "a_kernel_half_shootdown_drops_a_global_kernel_translation",
        metal::Metal {
            arms: &[metal::Arm {
                features: ACTUATOR_KERNEL,
                ..metal::once("shared-debug", "tests/images/testcases.toml", SELFTEST_PARAMS, &["test_rs_global_leaf_flush"])
            }],
            judge: |b| b[0].job_passed("test_rs_global_leaf_flush"),
        },
    ),
    (
        "a_user_half_shootdown_keeps_a_global_kernel_translation",
        metal::Metal {
            arms: &[metal::Arm {
                features: ACTUATOR_KERNEL,
                ..metal::once("shared-debug", "tests/images/testcases.toml", SELFTEST_PARAMS, &["test_rs_global_leaf_user_flush"])
            }],
            judge: |b| b[0].job_passed("test_rs_global_leaf_user_flush"),
        },
    ),
    (
        // What one diary record costs its writer: a million written back to
        // back by `SYS_DEBUG`'s flood, so the kernel that carries it, timed
        // with interrupts closed. Read, not held: the number is the product.
        "trace_record_cost",
        metal::Metal {
            arms: &[metal::Arm {
                features: ACTUATOR_KERNEL,
                ..metal::once("shared-debug", "tests/images/testcases.toml", SELFTEST_PARAMS, &["test_rs_trace_flood"])
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
        // A fed watchdog: the armed boot runs its whole list to its own stop,
        // which a chipset reset anywhere in it would have cut short.
        "watchdog_fed",
        metal::Metal {
            arms: &[metal::once("testcases-watchdog", "tests/images/testcases.toml", &["watchdog"], &[])],
            judge: |b| b[0].log_is_whole_to_its_stop(),
        },
    ),
    // ---- the boot facts, riding the same tests/images/testcases.toml image ----
    //
    // **Seven names and no extra minute of the machine.** Every needle below is a
    // record the *shipping* kernel writes on any boot it takes, so each rides
    // whatever image is already going on the bench; the two rows after them are
    // the two things this suite measures rather than reads, and those cost a
    // boot.
    (
        "smp_roster_and_tsc_trail",
        metal::Metal {
            arms: TESTCASES,
            // Each bench's local APIC in the mode its CPUs offer
            // ([`apic_mode_offered`]), which the declaration takes.
            judge: |b| {
                let (log, cpus) = (b[0].kernel(), b[0].cpus()?);
                smp_roster_and_tsc_trail(log.text(), cpus)?;
                apic_mode_held(&log, cpus, apic_mode_offered(log.text())?)
            },
        },
    ),
    (
        "topology_readback",
        metal::Metal { arms: TESTCASES, judge: |b| topology_readback(b[0].kernel().text(), b[0].cpus()?) },
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
                tsc_rate_at_boot(b[0].kernel().text())
            },
        },
    ),
    (
        "pci_inventory",
        metal::Metal { arms: TESTCASES, judge: |b| pci_inventory(b[0].kernel().text()) },
    ),
    (
        // The shipping kernel's W^X audit of its own tables, on the firmware
        // and memory map it was built for: the T14 and the AMD bench. Its
        // negative control is `kernel_wx_leaf_is_refused`, on QEMU.
        "kernel_wx",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| {
                let kernel = b[0].kernel();
                eprintln!("  [kernel_wx] {}", kernel.must_say(KERNEL_WX_HOLDS)?);
                Ok(())
            },
        },
    ),
    // ---- one image: tests/images/latencycase.toml armed with the shootdown bench ----
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
                in_acpi_mode_before_any_job(b[0])?;
                b[0].job_passed("test_rs_sched_stress")?;
                wake_latency_recorded(b[0])
            },
        },
    ),
    (
        // A ping-pong whose partner parks beside a spinner, with idle CPUs
        // beside them: the partner's wake lands on a busy CPU.
        "wake_onto_busy",
        metal::Metal { arms: TESTCASES, judge: |b| wake_onto_busy(b[0]) },
    ),
    (
        // Two builds and a helper-heavy job, each its own job, on a saturated
        // machine: each takes a third whatever its helper count. Its widths are
        // the benches', eight CPUs and sixteen.
        "job_fairness",
        metal::Metal { arms: TESTCASES, judge: |b| job_fairness(b[0]) },
    ),
    // ---- one image: tests/images/benchcase.toml, `osbench`'s ToyOS half ----
    (
        // The numbers compared with the same program on the bench's own Linux.
        // Evidence and not a verdict: they are read off the log, and the row
        // holds only that every group ran to its end on the bench's NVMe, on
        // a boot that did not spend itself formatting DATA. The T14 and the
        // AMD bench. One boot runs the whole suite, so three readbacks are
        // three runs.
        "osbench",
        metal::Metal {
            arms: BENCHCASE,
            judge: |b| {
                data_on_the_nvme(b[0])?;
                osbench_ran(b[0], &OSBENCH_GROUPS)
            },
        },
    ),
    (
        // `osbench energy`: the package's power, idle and under defined loads,
        // compared with the same program as root under the bench's Linux. A
        // boot of its own, since idle power is the whole machine's: nothing
        // else may run in it. Evidence and not a verdict, as `osbench` is; the
        // row holds that the idle row measured, which it does only where the
        // kernel reads the package's energy. The T14 and the AMD bench.
        "osbench_energy",
        metal::Metal { arms: OSBENCH_ENERGY, judge: |b| osbench_energy_measured(b[0]) },
    ),
    (
        // The clock's boot rate against the HPET's over the minutes the suite
        // runs, through its idle, busy and hot phases: the T14 and the AMD
        // bench, riding the `osbench` boot.
        "clock_holds_over_minutes",
        metal::Metal { arms: BENCHCASE, judge: |b| clock_holds_over_minutes(b[0]) },
    ),
    (
        // `osbench lan` against the `osbench-peer` the other bench's Linux runs,
        // compared with the same program under this bench's Linux on the same
        // path. A pass names it: the other bench has to be running the peer.
        // The T14 over its I219; the AMD bench has no card ToyOS drives.
        "osbench_lan",
        metal::Metal { arms: OSBENCH_LAN, judge: |b| osbench_ran(b[0], &["osbench_lan"]) },
    ),
    (
        // `osbench internet`: exploratory evidence over the public internet,
        // which no automated test depends on, so only a pass that names it runs
        // it. The T14; the AMD bench has no card ToyOS drives.
        "osbench_internet",
        metal::Metal { arms: OSBENCH_INTERNET, judge: |b| osbench_ran(b[0], &["osbench_internet"]) },
    ),
    (
        // What the PHY's ULP registers hold after the I219's disarm and the
        // full reset, which the stub only assumes. The T14 only: the AMD bench
        // has no card ToyOS drives. It rides `osbench`'s boot, whose i219 row
        // drives the card, and reads two of the driver's lines once its job
        // has seen the lease, which follows the bring-up.
        "i219_leaves_ulp_unarmed",
        metal::Metal { arms: BENCHCASE_ALONE, judge: |b| i219_leaves_ulp_unarmed(b[0]) },
    ),
    (
        // The I219's driver a process of its own: its link open at netstack,
        // up, and a lease from the bench's router carried over it, with no
        // link ending. The T14 only, for the same reason, on the same boot.
        "i219_carries_netstacks_link",
        metal::Metal { arms: BENCHCASE_ALONE, judge: |b| i219_carries_netstacks_link(b[0]) },
    ),
    // ---- tests/images/jobcase.toml, each image armed to end its boot its own way ----
    (
        // Its own boot, and it must not share one: it is the only arm in this
        // profile that deliberately leaves the machine unable to end its own
        // boot, and what it judges is that the machine ended it anyway.
        "boot_deadline_ends_a_wedge",
        metal::Metal {
            arms: &[metal::once("deadlinewedge", "tests/images/jobcase.toml", &["wedge-before-reset"], &[])],
            judge: |b| power::deadline_wedge_chain(&b[0].after_the_reset()?),
        },
    ),
    (
        // The same wedge under the chipset watchdog, with the deadline past
        // its bound: the watchdog's reset and not the kernel's ends the boot.
        // The AMD bench, whose FCH says which reset it was. Named only: no
        // TCO has reset the T14 (`issues/an-armed-tco-has-never-reset-the-t14.md`),
        // where this boot would wait out its deadline instead.
        "watchdog_resets",
        metal::Metal {
            arms: &[metal::Arm {
                named_only: true,
                ..metal::once("watchdogwedge", "tests/images/jobcase.toml", &["watchdog", "wedge-before-reset"], &[])
            }],
            judge: |b| power::watchdog_reset_chain(&b[0].kernel(), &b[0].log(), &b[0].after_the_reset()?),
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
            arms: &[metal::once("hardlockup", "tests/images/jobcase.toml", &["hard-lockup-probe"], &[])],
            judge: |b| power::hard_lockup_chain(&b[0].kernel(), &b[0].after_the_reset()?),
        },
    ),
    (
        // Its own boot, and it must not share one: it deliberately leaves the
        // page holding a record no image owns, and a boot that then read it as
        // a predecessor's is exactly what the arm above judges.
        "blackbox_foreign_record",
        metal::Metal {
            arms: &[metal::once(
                "foreignrecord",
                "tests/images/jobcase.toml",
                &[toyos_metal::FOREIGN_RECORD_ARM],
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
    // ---- one image: tests/images/proctreecase.toml ----
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
        // A connection's badge is the minted bytes, read back by its own
        // port's acceptor alone.
        "port_badge",
        metal::Metal { arms: PROCTREECASE, judge: |b| b[0].job_passed("test_rs_port_badge") },
    ),
    (
        // An installed package sees its own directory read-only and its own
        // folder as `HOME`, and nothing else of `/apps` or `/home`.
        "app_view",
        metal::Metal { arms: PROCTREECASE, judge: |b| app_view(b[0]) },
    ),
    // ---- one image: tests/images/metalcase.toml, whose one job is the hold ----
    //
    // The rows after the first read what any boot that hands the machine back
    // writes, so they ride this one.
    (
        "metal_sim_scanout_wc",
        metal::Metal { arms: METALCASE, judge: |b| scanout_wc(b[0].kernel().text()) },
    ),
    (
        // The panel's EDID from the firmware's GOP to the compositor's scale,
        // and the desktop and a client drawn at it: the loader's read before
        // `ExitBootServices` is no host test's, and QEMU's OVMF publishes no
        // EDID protocol.
        "gop_edid_scale",
        metal::Metal {
            arms: METALCASE_SCALE,
            judge: |b| {
                b[0].job_passed("test_rs_scale_window")?;
                gop_edid_scale(b[0].loader().text(), b[0].log().text(), b[0].kernel().text())
            },
        },
    ),
    (
        // The machine's own i8042 and EC, driven from userland: the keyboard
        // reset and armed set 2 translated, its `0xAA`'s time named, the aux
        // device armed or found absent by name, and no i8042 source in the
        // kernel's interrupt census.
        "ps2server_arms",
        metal::Metal { arms: METALCASE, judge: |b| ps2server::armed(&b[0].log(), &b[0].kernel()) },
    ),
    (
        // The patch the loader found on ROOT, and every CPU's microcode line,
        // held to what its bench is held to: `microcode_on_metal` says which
        // machine is held to what. Metal alone: no host model has either
        // vendor's patch loader, and a QEMU guest's microcode is the
        // hypervisor's. The T14 and the AMD bench.
        "microcode",
        metal::Metal {
            arms: METALCASE,
            judge: |b| microcode_on_metal(b[0].loader().text(), b[0].kernel().text(), b[0].cpus()?),
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
    (
        // The T14's display driver offers the compositor the firmware's
        // output on `display-output`, and the compositor's presents are
        // answered at their vblanks, which only the iGPU's interrupt raises.
        // Beside it `hda` still plays to the analog codec with the display
        // engine's PG3, which holds the display codec, powered down. The T14
        // alone: no other bench has a Tiger Lake iGPU.
        "display_output_presents",
        metal::Metal { arms: METALCASE_TONE, judge: |b| display_output_presents(b[0]) },
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
        "lapic_spurious_vector",
        metal::Metal { arms: SELFTESTS, judge: |b| lapic_vectors(b[0].kernel().text()) },
    ),
    (
        // No drain on this side: the whole boot's records are on the log, so
        // the probe's line is either in them or it never ran.
        "sysret_ss_reload",
        metal::Metal { arms: SELFTESTS, judge: |b| sysret_ss(b[0].kernel().text()) },
    ),
    (
        // A unit handed over translating, remapping and queueing, or on an
        // AMD-Vi machine blocking every DMA with its command buffer and event
        // log running, on real silicon: each field goes off by its own write
        // before the unit is programmed or left off, and the boot goes on.
        "iommu_firmware_left",
        metal::Metal { arms: FIRMWARE_LEFT, judge: |b| iommu_firmware_left(b[0].kernel().text()) },
    ),
    // ---- a claimed function at the unit, on tests/images/testcases.toml ----
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
        // The I219's interrupts on the function itself, which QEMU's e1000e
        // is not: no MSI-X capability, causes forced through `ICS` counted on
        // the claim's MSI vector unmasked and masked, `ICR`'s asserted read
        // under auto-mask, and the driver under traffic on the bench's wire.
        // It shares the boot with `claim_reuses_its_remapping_entry`, whose
        // judge reads only its own job's records.
        "i219_messages_on_its_msi_vector",
        metal::Metal {
            arms: &[metal::once("testcases", "tests/images/testcases.toml", &[], &[claims::MSI])],
            judge: |b| {
                b[0].job_passed(claims::MSI)?;
                claims::messages_on_its_msi_vector(&b[0].kernel(), &b[0].log())
            },
        },
    ),
    (
        // Every device address a domain maps is outside every root-bridge
        // window firmware declared.
        "domains_map_clear_of_the_host_bridges",
        metal::Metal { arms: TESTCASES, judge: |b| claims::clear_of_host_bridges(&b[0].kernel()) },
    ),
    (
        // An AMD-Vi unit programmed, and the boot disk's controller read
        // through a domain of its own: the register sequence, the device
        // table and the host tables on real silicon. The AMD bench's row.
        "amdvi_translates_the_boot_controller",
        metal::Metal { arms: TESTCASES, judge: |b| amdvi_translates(&b[0].kernel(), &b[0].log()) },
    ),
    (
        // An AMD-Vi unit remapping every message: the boot disk's
        // controller's interrupt delivered through an entry of its own
        // table, on real silicon, and every I/O APIC named.
        "amdvi_remaps_the_boot_controller",
        metal::Metal { arms: TESTCASES, judge: |b| amdvi_remaps(&b[0].kernel(), &b[0].log()) },
    ),
    (
        // The AMD bench's first xHCI, which the image's usbd row leaves,
        // claimed and driven by usbd through an entry of its own table, its
        // MSI-X table in the register BAR it hands over; then aimed past its
        // grant, recorded against the claim; then its holder rewriting its
        // own table entry to a vector not its own, refused and recorded.
        "amdvi_usbd_drives_its_own_entry",
        metal::Metal {
            arms: &[metal::once(
                "testcases",
                "tests/images/testcases.toml",
                &[],
                &[
                    "test_rs_usbd_spare",
                    "test_rs_usbd_spare_fault",
                    "test_rs_end_returns_claims",
                    "test_rs_usbd_spare_vector",
                ],
            )],
            judge: |b| {
                for job in ["test_rs_usbd_spare", "test_rs_usbd_spare_fault", "test_rs_end_returns_claims", "test_rs_usbd_spare_vector"] {
                    b[0].job_passed(job)?;
                }
                amdvi_usbd(&b[0].kernel(), &b[0].log(), &b[0].after_the_reset()?)
            },
        },
    ),
    (
        // Each radio P1 names (`issues/there-is-no-wifi.md`) that the machine
        // has, claimed with its BARs where firmware put them, and the MSI-X
        // entry the kernel armed read back through its holder's mapping of
        // the table's page: the T14's AX210, the AMD bench's AX200 behind its
        // bridge. It rides the `testcases` boot.
        "msix_table_read_in_place",
        metal::Metal {
            arms: &[metal::once("testcases", "tests/images/testcases.toml", &[], &["test_rs_msix_in_place"])],
            judge: |b| {
                b[0].job_passed("test_rs_msix_in_place")?;
                for line in msix_read_in_place(b[0].kernel().text(), b[0].log().text())? {
                    eprintln!("  [claims] {line}");
                }
                Ok(())
            },
        },
    ),
    (
        // The bench's own NVMe disk, which the boot came off, driven by the
        // row's class: nothing on it is touched but the log and boot volumes
        // the loader named and its ToyOS DATA partition, which is left alone
        // while it is foreign and formatted or mounted once it is designated
        // (`hostdisk::left_alone`), and never stood in for by memory. The T14
        // and the AMD bench; the metal loop's next boot of the host off that
        // disk is the other half.
        "nvme_leaves_the_benchs_disk_alone",
        metal::Metal {
            arms: TESTCASES,
            judge: |b| common::hostdisk::left_alone(&b[0].log(), &common::hostdisk::loader_named(&b[0].loader())?).map(drop),
        },
    ),
    (
        // The bench's DATA partition designated from inside ToyOS, and the
        // next boot's DATA file server formatting it and mounting it: the
        // driver of the NVMe disk judges the partition, writes the stamp,
        // flushes and reads it back, under the one authorization the
        // designation image grants its runner. Destructive and once per
        // bench, so named only: the T14 and the AMD bench, whose p4s are the
        // owner's empty ToyOS DATA.
        "bench_data_designated",
        metal::Metal { arms: DESIGNATE, judge: |b| bench_data_designated(&b[0].log(), &b[1].log()) },
    ),
    (
        // The log disk written while the boot deadline resets the machine: the
        // pass after the reset reads the deadline's record and the host boots
        // again off the disk the write was in flight on. It is the kernel's
        // reset path that is judged; the job is the load.
        "the_disk_survives_a_reset_under_load",
        metal::Metal {
            arms: &[metal::once("usbload", "tests/images/testcases.toml", &[], &[metal::STICK_LOAD_JOB])],
            judge: |b| power::the_disk_survives_a_reset_under_load(b[0]),
        },
    ),
    (
        // The T14's first xHCI, the Type-C one, which the image's usbd row
        // leaves, claimed, and usbd driving it: handed
        // over, MSI armed through remapping, and the No-Op answered by an
        // interrupt; usbd is killed, so the release says which reset the
        // function advertises. Then a holder of it is killed and the claim
        // asked for again at once, each round: its end follows its release.
        "usbd_drives_the_type_c_controller",
        metal::Metal {
            arms: &[metal::once(
                "testcases",
                "tests/images/testcases.toml",
                &[],
                &["test_rs_usbd_spare", "test_rs_end_returns_claims"],
            )],
            judge: |b| {
                b[0].job_passed("test_rs_usbd_spare")?;
                usbd_on_metal(&b[0].kernel(), &b[0].log())?;
                b[0].job_passed("test_rs_end_returns_claims")?;
                ends_returned_claims(b[0].log().text()).map(drop)
            },
        },
    ),
    // ---- the `isa` claim: the machine's i8042, driven by a test process ----
    (
        // The I/O permission bitmap on the machine's own processor: the ports
        // open to the process that bound them, and every other access killed
        // by name.
        "isa_ports_are_the_binders_alone",
        metal::Metal {
            arms: ISA,
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
            arms: ISA,
            judge: |b| {
                b[0].job_passed("test_rs_isa_lines")?;
                isa::lines(&b[0].kernel())
            },
        },
    ),
    (
        // A port of the row the kernel declared, as a FADT reset register at
        // 0x64 is: the claim is refused naming who holds it.
        "isa_claim_refused_on_a_kernel_port",
        metal::Metal {
            arms: &[metal::once(
                "isa-port-declared",
                "tests/images/testcases.toml",
                &["isa-port-declared"],
                &["test_rs_isa_claim_refused"],
            )],
            judge: |b| {
                b[0].job_passed("test_rs_isa_claim_refused")?;
                isa::refused(&b[0].kernel())
            },
        },
    ),
    (
        "crash_report_reads_no_kernel_memory",
        metal::Metal {
            arms: &[metal::once("testcases", "tests/images/testcases.toml", &[], &["test_rs_fault_gates"])],
            judge: |b| {
                b[0].job_passed("test_rs_fault_gates")?;
                faults::crash_report_reads_no_kernel_memory(&b[0].kernel())
            },
        },
    ),
    // ---- one image: tests/images/testcases.toml, ring and the parity instrument alone ----
    (
        // `toyos-crypto` beside ring on the bench's own CPU, both builds'
        // tables printed: that ring runs at all, and that every row measured.
        // The T14 and the AMD bench.
        "crypto_instrument",
        metal::Metal {
            arms: CRYPTOCASE,
            judge: |b| {
                for line in crypto_table(b[0])?.render() {
                    eprintln!("  [crypto] {line}");
                }
                Ok(())
            },
        },
    ),
    (
        // `toyos-crypto`'s secret routines timed fixed against random on the
        // bench's own CPU (stage 4 of
        // `issues/toyos-crypto-is-in-the-tree-on-par-with-ring-and-secure.md`):
        // each routine's verdict is printed and recorded there, and decides
        // nothing here. The job fails only where its leaky control goes
        // unseen. The T14 and the AMD bench.
        "crypto_secret_timing",
        metal::Metal { arms: CRYPTO_CT, judge: |b| crypto_ct_lines(b[0], "test_rs_crypto_constant_time") },
    ),
    (
        // The same of the NIST curves and Ed25519, whose million
        // measurements a routine take the bench for hours, in a boot of
        // their own. The T14 and the AMD bench.
        "crypto_secret_timing_curves",
        metal::Metal {
            arms: CRYPTO_CT_CURVES,
            judge: |b| crypto_ct_lines(b[0], "test_rs_crypto_constant_time_curves"),
        },
    ),
];

/// What `crypto_constant_time` adds to its list's bound: its job took 970 s
/// on the T14 and 822 s on the AMD bench (at 7a36687d0), and half as much
/// again.
const CRYPTO_CT_MS: u64 = 1_455_000;

/// What `crypto_constant_time_curves` adds: its job took 7,036 s on the T14
/// and 6,234 s on the AMD bench (at 7a36687d0), and half as much again.
const CRYPTO_CT_CURVES_MS: u64 = 10_554_000;

/// Each constant-time job alone in its boot, which nothing else shares: a
/// job beside it would be noise in what it times.
const CRYPTO_CT: &[metal::Arm] = &[metal::Arm {
    adds_ms: CRYPTO_CT_MS,
    named_only: true,
    ..metal::once("cryptoctcase", "tests/images/testcases.toml", &[], &["test_rs_crypto_constant_time"])
}];
const CRYPTO_CT_CURVES: &[metal::Arm] = &[metal::Arm {
    adds_ms: CRYPTO_CT_CURVES_MS,
    named_only: true,
    ..metal::once("cryptoctcurvescase", "tests/images/testcases.toml", &[], &["test_rs_crypto_constant_time_curves"])
}];

/// A constant-time job's lines, once it has passed.
fn crypto_ct_lines(boot: &metal::Readback, job: &str) -> Result<(), String> {
    boot.job_passed(job)?;
    let log = boot.log();
    let lines: Vec<&str> =
        log.text().lines().filter_map(toyos_logstream::program_line).map(|said| said.text).filter(|t| t.starts_with("ct ")).collect();
    if lines.is_empty() {
        return Err(format!("{job} passed and printed no verdict"));
    }
    for line in lines {
        eprintln!("  [ct] {line}");
    }
    Ok(())
}

/// The jobs of the crypto boot, in order: ring's known answers, then the
/// instrument as it ships and with debug assertions off.
const CRYPTO_JOBS: [&str; 3] = ["test_rs_ring_kat", "test_rs_crypto_parity", "test_rs_crypto_parity_unasserted"];

/// Ring and the parity instrument with no other job in their boot, which
/// would compete with what it times.
const CRYPTOCASE: &[metal::Arm] = &[metal::once("cryptocase", "tests/images/testcases.toml", &[], &CRYPTO_JOBS)];

/// Both builds' parity table, once every job of the crypto boot has passed.
fn crypto_table(boot: &metal::Readback) -> Result<toyos_parity::Table, String> {
    for job in CRYPTO_JOBS {
        boot.job_passed(job)?;
    }
    let log = boot.log();
    toyos_parity::read(log.text().lines().filter_map(toyos_logstream::program_line).map(|said| said.text))
}

/// The plain `tests/images/testcases.toml` boot, which starts no ps2server, so
/// the one grantable row is free: the ports' job first, since its last holder
/// keeps them until it ends.
const ISA: &[metal::Arm] = &[metal::once(
    "isa",
    "tests/images/testcases.toml",
    &[],
    &["test_rs_isa_grant", "test_rs_isa_lines"],
)];

/// The boot most rows ride, and every shipping-kernel member behind them
/// ([`shared_metal`]): the plain `tests/images/testcases.toml` shape.
const TESTCASES: &[metal::Arm] = &[metal::once(
    "testcases",
    "tests/images/testcases.toml",
    &[],
    &[
        "test_rs_wake_storm_cost",
        "test_rs_wake_onto_busy",
        "test_rs_audio_tone",
        "test_rs_audio_tone_resampled",
        audio::TONE_BESIDE_LOG_WRITE,
        "test_rs_volume_step",
        "test_rs_syscall_cost",
        "test_rs_spawn_cost",
        "test_rs_job_fairness",
        claims::RECLAIM,
    ],
)];

/// `osbench`'s groups, each a job named by its symlink, in `osbench all`'s
/// order: `boot` first, which reads how long the boot took to reach it, and
/// `crunch` last, which heats the machine.
const OSBENCH_GROUPS: [&str; 16] = [
    "osbench_boot",
    "osbench_identity",
    "osbench_system",
    "osbench_sched",
    "osbench_fair",
    "osbench_files",
    "osbench_ipc",
    "osbench_net",
    "osbench_procs",
    "osbench_mem",
    "osbench_time",
    "osbench_work",
    "osbench_tail",
    "osbench_nvme",
    "osbench_persist",
    "osbench_crunch",
];

/// The suite runs past every boot's bound on purpose: `tail` alone waits out
/// 100 s of wakes and spinners. Six boots of `e5573b7fe` ended their last job
/// at 175 to 191 s on five, and at 244 s on the T14's first, where one
/// served 64 MiB write stalled and `files` took 80 s against 13. The groups
/// since add at most 50 s by their own spans: `crunch`'s one-thread 16 s
/// (ToyOS reads no temperature, so its all-core phase does not run), and
/// under 30 s of memcpy sizes, pipe sizes and 1 ms timers. Each of the some
/// ninety benchmarks that settle the clock settles it before and after
/// itself, 50 ms where the clock holds and 0.5 s at most (`clock::settle`):
/// under 95 s. So a list bound of 525 s leaves that stall 136 s.
const BENCHCASE: &[metal::Arm] = &[metal::Arm {
    adds_ms: 465_000,
    ..metal::once("benchcase", "tests/images/benchcase.toml", &[], &OSBENCH_GROUPS)
}];

/// `energy` waits up to 120 s for the machine to go quiet, then measures 60 s
/// idle and three 30 s loads, settling the clock before and after each spin:
/// under 160 s more. 300 s leaves its quiet wait 20 s over its own bound.
const OSBENCH_ENERGY: &[metal::Arm] = &[metal::Arm {
    adds_ms: 300_000,
    ..metal::once("energybenchcase", "tests/images/benchcase.toml", &[], &["osbench_energy"])
}];

/// The two groups that need another party share one boot when one pass names
/// both. Each adds its own worst span; the 60 s every list has is the
/// boot's margin, not either group's.
///
/// `lan` looks for its peer for up to 60 s, inside which the boot's lease
/// lands, then measures for under 60 s over a Wi-Fi hop: 2,155 round trips
/// at up to 10 ms and twelve 3 s transfers with their warm-ups.
const OSBENCH_LAN: &[metal::Arm] = &[metal::Arm {
    adds_ms: 120_000,
    named_only: true,
    ..metal::once("netbenchcase", "tests/images/benchcase.toml", &[], &["osbench_lan"])
}];
/// `internet` waits up to 60 s for the lease when it runs first, then
/// measures four 10 MB downloads, four 10 MB uploads and 77 short exchanges:
/// under 45 s at 3 MB/s down and 1.5 MB/s up. Those rates are an assumed
/// floor, far under the T14's first ToyOS readings, and a slower link ends
/// the list at the bound.
const OSBENCH_INTERNET: &[metal::Arm] = &[metal::Arm {
    adds_ms: 105_000,
    named_only: true,
    ..metal::once("netbenchcase", "tests/images/benchcase.toml", &[], &["osbench_internet"])
}];

/// [`BENCHCASE`]'s boot, with one job: the lease landing, which follows the
/// card's bring-up and every line it logged. Without it the list's `reboot`
/// lands while the driver is still reaching the PHY.
const BENCHCASE_ALONE: &[metal::Arm] = &[metal::once("benchcase", "tests/images/benchcase.toml", &[], &["test_rs_lan_lease"])];

/// The I219 driver's account of its part's start: what its wake found of
/// Ultra Low Power and disarmed, and the configuration its bring-up read back
/// unarmed. Both lines are printed, because what was found is what the next
/// boot's diagnosis reads.
fn i219_leaves_ulp_unarmed(boot: &metal::Readback) -> Result<(), String> {
    boot.job_passed("test_rs_lan_lease")?;
    let log = boot.log();
    let woke = log.must_say("i219: before the reset the PHY was asked:")?;
    let left = log.must_say("it is left with ULP configuration")?;
    eprintln!("  [i219] {woke}");
    eprintln!("  [i219] {left}");
    if !woke.contains(", and disarmed") {
        return Err(format!("the I219's wake did not disarm ULP: {woke}"));
    }
    if !left.contains(" (unarmed, ") {
        return Err(format!("the I219's bring-up left ULP armed: {left}"));
    }
    Ok(())
}

/// The I219's link as netstack holds it: opened from the driver's hello, up,
/// and a lease carried over it, with no link ending in the boot.
fn i219_carries_netstacks_link(boot: &metal::Readback) -> Result<(), String> {
    boot.job_passed("test_rs_lan_lease")?;
    let log = boot.log();
    log.must_say("netstack: i219's link is open")?;
    let up = log.must_say("i219: link up at ")?;
    log.must_say("netstack: DHCP: lease ")?;
    log.must_not_say("'s link ended")?;
    eprintln!("  [i219] {up}");
    Ok(())
}

/// Every group's job exited 0, which `osbench` does only once each of its
/// benchmarks measured, and said it was done.
fn osbench_ran(boot: &metal::Readback, jobs: &[&str]) -> Result<(), String> {
    in_acpi_mode_before_any_job(boot)?;
    for &job in jobs {
        boot.job_passed(job)?;
        let group = job.strip_prefix("osbench_").expect("a group's job");
        boot.log().must_say(&format!("group={group} done"))?;
    }
    Ok(())
}

/// DATA, which `osbench persist` writes, was a volume of ours on the bench's
/// NVMe, mounted (`hostdisk::left_alone`, which also refuses DATA from
/// memory). The boot that formatted it is no measured run: its start went to
/// the format, and the volume it leaves is new; it says how long that took.
fn data_on_the_nvme(boot: &metal::Readback) -> Result<(), String> {
    let log = boot.log();
    let ours = common::hostdisk::loader_named(&boot.loader())?;
    match common::hostdisk::left_alone(&log, &ours)?.1 {
        common::hostdisk::Data::Mounted => Ok(()),
        common::hostdisk::Data::Formatted => {
            let text = log.text();
            let at = |said: &str| text.lines().find(|l| l.contains(said)).and_then(since_boot);
            let took = at("formatting it").zip(at("fileserver: Data serving ")).map(|(start, end)| end - start);
            let took = took.map_or("its end unlogged".to_string(), |s| format!("{s:.3} s"));
            Err(format!("this boot formatted DATA off its designation ({took}), and the format's boot is no measured run"))
        }
        common::hostdisk::Data::Foreign => {
            Err("DATA on the bench's NVMe was no volume of ours, so persist measured no disk".into())
        }
    }
}

/// A log line's seconds since boot: `[2026-10-24 14:17:17 12.664 fileserver]`.
fn since_boot(line: &str) -> Option<f64> {
    line.strip_prefix('[')?.split_whitespace().nth(2)?.parse().ok()
}

/// `osbench energy` ran in ACPI mode, and its idle row measured the package.
fn osbench_energy_measured(boot: &metal::Readback) -> Result<(), String> {
    osbench_ran(boot, &["osbench_energy"])?;
    boot.log().must_say("bench=energy.idle.package unit=mW ")?;
    Ok(())
}

/// A timing boot was in ACPI mode before its first job and stayed there, as a
/// shipped image and the bench's Linux are. In legacy mode the T14's firmware
/// stops every CPU every 2.2 s, and its time would be measured as ToyOS's
/// (`issues/the-t14s-firmware-interrupts-every-cpu-every-2-2-s-under-toyos.md`).
fn in_acpi_mode_before_any_job(boot: &metal::Readback) -> Result<(), String> {
    let log = boot.log();
    let text = log.text();
    let first_job = text.find("===TEST_START").ok_or("the boot started no job")?;
    let in_acpi_mode = ["the firmware handed over in ACPI mode", "acpi: ACPI mode: ACPI_ENABLE "]
        .iter()
        .filter_map(|said| text.find(said))
        .min();
    if !in_acpi_mode.is_some_and(|at| at < first_job) {
        return Err("the machine was not in ACPI mode before the first job: the boot measured the firmware's legacy mode".to_string());
    }
    if let Ok(left) = log.must_say("acpi: legacy mode again") {
        return Err(format!("the machine left ACPI mode inside the boot: {left}"));
    }
    Ok(())
}

/// The same boot, its rows ended on the hold, which waits for the ACPI
/// server's power sources, the last line of its start. The members run
/// behind it: the hold gives up at a time since boot, which a list of
/// members before it would spend.
const TESTCASES_HELD: &[metal::Arm] = &[metal::Arm {
    last: Some("test_rs_acpi_hold"),
    ..metal::once("testcases", "tests/images/testcases.toml", &[], &[])
}];

/// Two boots of the designation image (`tests/images/designatecase.toml`):
/// the first designates the machine's DATA partition, the second only
/// reboots, which is where the file server formats what the first stamped.
const DESIGNATE: &[metal::Arm] = &[
    metal::Arm {
        named_only: true,
        ..metal::once("designate", "tests/images/designatecase.toml", &[], &["disks designate data --commit"])
    },
    metal::Arm { named_only: true, ..metal::once("designated", "tests/images/designatecase.toml", &[], &[]) },
];

/// Its own boot: `kernel_objects_refused` holds every frame a process may, so
/// every other process's allocation is refused while it runs.
const TESTCASES_REFUSED: &[metal::Arm] =
    &[metal::once("testcases-refused", "tests/images/testcases.toml", &[], &["test_rs_kernel_objects_refused"])];

/// Its own boot: `memory_floor` holds every frame an ordinary job may, on
/// `tests/images/floorcase.toml`'s floors. No key is typed on the bench, so
/// the input arm is QEMU's alone.
const FLOORCASE: &[metal::Arm] =
    &[metal::once("floorcase", "tests/images/floorcase.toml", &[], &["test_rs_memory_floor"])];

/// Its own boot: `readdir_bound` fills `/tmp` to the VFS listing limit, and
/// every later `read_dir("/tmp")` in a boot it shared would be refused.
const TESTCASES_READDIR: &[metal::Arm] =
    &[metal::once("testcases-readdir", "tests/images/testcases.toml", &[], &["test_rs_readdir_bound"])];

/// The shipping kernel with the windows' instrument and nothing else, so what
/// it reads is that kernel under the herd. Three exits, a report each:
/// `idle_span`'s is the boot's first, where the kernel holds; `pwd`'s reads the
/// hold back and empties every record; the herd's is the herd's reading.
const WINDOWSCASE: &[metal::Arm] = &[metal::Arm {
    features: toyos_build::build::MASK_WINDOWS_KERNEL,
    ..metal::once(
        "windowscase",
        "tests/images/testcases.toml",
        &[],
        &["test_rs_idle_span", "pwd", WINDOWS_LOAD, LOG_WRITE],
    )
}];

/// The load `mask_windows` reads the windows under.
const WINDOWS_LOAD: &str = "test_rs_ring_park_herd";

/// The log-write load it reads them under too (#901 §9): 8 MiB through the
/// log disk's driver, whose interrupts and threads are a claim's and a
/// process's.
const LOG_WRITE: &str = "test_rs_log_write";

/// The head of the kernel's record of `job`'s exit.
fn exited(job: &str) -> String {
    format!("{}{} pid=", toyos_build::bootlog::EXIT, toyos_build::bootlog::recorded_name(job))
}

/// What [`display_output_presents`] reads off the T14's metalcase boot:
/// - the driver's DMC line, with DC6 allowed on DDI A;
/// - its hello taken and a present answered, at a later frame of pipe A's
///   counter than the allow, so the timing generator ran through DC6 being
///   allowed;
/// - nothing dropped;
/// - `tone_after_dc6`'s tone played whole to `hda`'s output, which it starts
///   only once PG3 is down.
///
/// Whether `hda` walked its codecs before or after PG3 went down is said,
/// since only the later order tests the display codec's power.
fn display_output_presents(boot: &metal::Readback) -> Result<(), String> {
    let log = boot.log();
    let frame = |line: &str, after: &str| -> Result<u64, String> {
        line.split_once(after).and_then(|(_, n)| n.trim().parse().ok()).ok_or_else(|| format!("no frame number in {line:?}"))
    };
    let dc6 = log.must_say("tgl-display: DMC package")?;
    if !dc6.contains("DDI select 0x1;") || !dc6.contains(" 00000002, AUDIO_PIN_BUF_CTL") {
        return Err(format!("DC6 was not allowed on DDI A: {dc6}"));
    }
    let answered = log.must_say("tgl-display: first present answered at pipe A frame")?;
    let (allowed_at, answered_at) = (frame(dc6, "pipe A frame")?, frame(answered, "pipe A frame")?);
    if answered_at <= allowed_at {
        return Err(format!("pipe A's frame counter read {allowed_at} at the allow and {answered_at} at the first answer"));
    }
    log.must_say("compositor: a display driver scans out the 1920x1080 output")?;
    log.must_say("compositor: the display driver answered its first present")?;
    for refused in ["compositor: dropping the display driver", "tgl-display: presents go unanswered"] {
        if let Some(line) = log.text().lines().find(|line| line.contains(refused)) {
            return Err(format!("the driver's presents stopped: {line}"));
        }
    }
    boot.job_passed("test_rs_tone_after_dc6")?;
    audio::tone_played(&log, "test_rs_tone_after_dc6")?;
    let at = |line: &str| log.text().find(line).expect("a line said is in the log");
    let offered = log.must_say("offered to soundserver")?;
    let order = if at(offered) > at(dc6) { "after" } else { "before" };
    println!(
        "display_output_presents: pipe A frames {allowed_at} -> {answered_at}; hda offered its output {order} PG3 went down, and the tone played after"
    );
    Ok(())
}

/// `tests/images/metalcase.toml`, ended once ps2server's probe has: a boot
/// that ended sooner would cut off what `ps2server_arms` reads.
const METALCASE: &[metal::Arm] = &[metal::Arm {
    last: Some("test_rs_ps2server_hold"),
    ..metal::once("metalcase", "tests/images/metalcase.toml", &[], &[])
}];

/// [`METALCASE`] with `scale_window`'s logical window, which the compositor
/// gives a buffer at the panel's scale.
const METALCASE_SCALE: &[metal::Arm] = &[metal::Arm {
    last: Some("test_rs_ps2server_hold"),
    ..metal::once("metalcase", "tests/images/metalcase.toml", &[], &["test_rs_scale_window"])
}];

/// [`METALCASE`] with `tone_after_dc6`, which `display_output_presents`
/// plays once the display driver has PG3 down, muted like every metal tone.
const METALCASE_TONE: &[metal::Arm] = &[metal::Arm {
    last: Some("test_rs_ps2server_hold"),
    ..metal::once("metalcase", "tests/images/metalcase.toml", &[], &["test_rs_tone_after_dc6"])
}];

/// A launcher and a declared `cat` and shell, which `process_tree`'s subtree
/// launches, a `toybox` row holding `roster`, which `launch_toctou` races, and
/// the `/apps` `app_view` launches its package from.
const PROCTREECASE: &[metal::Arm] = &[metal::once(
    "proctreecase",
    "tests/images/proctreecase.toml",
    &[],
    &["test_rs_process_tree", "test_rs_launch_toctou", "test_rs_port_badge", "test_rs_app_view"],
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
    "lapic-spurious-selftest",
    "unclaimed-vector-selftest",
    "sysret-ss-probe",
];

const SELFTESTS: &[metal::Arm] = &[metal::Arm {
    features: ACTUATOR_KERNEL,
    ..metal::once("shared-debug", "tests/images/testcases.toml", SELFTEST_PARAMS, &[])
}];

/// Its own boot: on the T14 the hand-over it stages can halt the machine on
/// the iGPU's fault
/// (`issues/the-t14s-igpu-faults-read-permission-after-translation-goes-off-and-on-under-its-scanout.md`),
/// which would take every row batched with it.
const FIRMWARE_LEFT: &[metal::Arm] = &[metal::Arm {
    features: ACTUATOR_KERNEL,
    ..metal::once("iommu-firmware-left", "tests/images/testcases.toml", &["iommu-firmware-left"], &[])
}];

/// The two boots every discovered member rides on the T14, each behind its
/// rows' jobs: `testcases`, the shipping kernel's, carries every discovered
/// Rust binary, then the C corpus, then [`LAST_MEMBERS`]; `shared-debug`
/// carries [`ACTUATOR_TESTS`] on the kernel that has `SYS_DEBUG`, armed with
/// [`SELFTEST_PARAMS`]. A second boot rather than one image for the whole
/// set: what those need is a syscall number the rest must not have.
fn shared_metal(c_bins: &[(String, Vec<u8>)]) -> Vec<metal::SharedBoot> {
    let rust = |names: Vec<String>| {
        names.into_iter().map(|n| metal::Member { job: format!("test_rs_{n}"), adds_ms: toyos_tco::RUST_MEMBER_MS })
    };
    let (debug, shipping): (Vec<String>, Vec<String>) = discover_rust_tests()
        .into_iter()
        .partition(|name| ACTUATOR_TESTS.contains(&name.as_str()));
    let corpus = c_corpus_metal(c_bins);
    let last = rust(LAST_MEMBERS.iter().map(ToString::to_string).collect());
    vec![
        metal::SharedBoot { members: rust(shipping).chain(corpus.members.clone()).chain(last).collect(), ..corpus },
        metal::SharedBoot {
            boot: "shared-debug".to_string(),
            config: "tests/images/testcases.toml",
            params: SELFTEST_PARAMS,
            features: ACTUATOR_KERNEL,
            members: rust(debug).collect(),
            files: Vec::new(),
            links: Vec::new(),
        },
    ]
}

/// **One boot for both measurements this suite takes rather than reads.**
/// `tests/images/latencycase.toml` is the only config that endows the real-time band, and
/// the TLB bench is a boot parameter rather than a job — so arming this image
/// with it costs the machine nothing and saves a whole minute. The bench runs
/// on the BSP between the roster's release and the idle loop, before either job
/// starts, so what it spends is boot time and not latency.
const LATENCYCASE: &[metal::Arm] = &[metal::once(
    "latencycase",
    "tests/images/latencycase.toml",
    &["tlb-shootdown-bench"],
    &["test_rs_cyclictest", "test_rs_sched_stress"],
)];

/// The C corpus's part of the `testcases` boot, which [`shared_metal`] puts
/// the Rust members around: one job per case, each judged in the guest.
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
    let mut members = Vec::new();
    let mut files = Vec::new();
    let mut links = Vec::new();
    for (case, data) in c_bins {
        // A case with no committed expectation is one nothing could judge, and
        // shipping it would be a job that passes by comparing nothing.
        let Some(expected) = c_expectation(case) else { continue };
        files.push((format!("expect/{case}"), expected.into_bytes()));
        files.push((format!("bin/test_c_{case}"), data.clone()));
        links.push((format!("bin/{case}"), format!("/system/bin/{CCHECK}")));
        members.push(metal::Member { job: case.clone(), adds_ms: toyos_tco::C_MEMBER_MS });
    }
    metal::SharedBoot { boot: "testcases".to_string(), config: "tests/images/testcases.toml", params: &[], features: &[], members, files, links }
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
        why: Why::Declined("`libc panic: longjmp not implemented` (`sdk/libc/src/misc.rs`), exit 134. The reason the old skip list gave for this pair — setjmp — is the one claim of its kind that turned out to be right"),
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

/// Discover C tests by scanning tests/corpus/tinycc/*.c.
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

/// Discover the Rust test binaries: every one but the [`RUST_SKIP`] helpers
/// and the [`LAST_MEMBERS`], which [`shared_metal`] places itself.
///
/// **A name that arrives this way is registered by nothing but its file.**
/// `tests/guest/rust-tests/src/bin/<name>.rs` is the whole declaration — no row
/// here names it.
fn discover_rust_tests() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/guest/rust-tests/src/bin");
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| {
            let path = e.ok()?.path();
            let name = path.file_stem()?.to_str()?.to_string();
            let placed = RUST_SKIP.contains(&name.as_str()) || LAST_MEMBERS.contains(&name.as_str());
            (path.extension()? == "rs" && !placed).then_some(name)
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

/// `tests/guest/rust-tests`' binary that `tests/images/virtjobcase.toml` runs as its job
/// `test_rs_abuse_readonly_copyout`.
const VIRT_COPYOUT: &str = "abuse_readonly_copyout";

/// The same for its job `test_rs_ring0_timer_in_syscall`.
const VIRT_RING0_TIMER: &str = "ring0_timer_in_syscall";

/// `tests/guest/rust-tests`' binary that `tests/images/virtsmpcase.toml` runs as its job
/// `test_rs_counters_read`.
const VIRT_COUNTERS_READ: &str = "counters_read";

/// `tests/guest/rust-tests`' binary that `tests/images/virtsmpcase.toml` runs as its job
/// `test_rs_trace_read`.
const VIRT_TRACE_READ: &str = "trace_read";

/// The same for its job `test_rs_timing_protection`.
const VIRT_TIMING: &str = "timing_protection";

/// `tests/guest/rust-tests`' binary `name` built for `arch`, once a run, as a
/// file a case's job list names on ROOT.
fn suite_bin(arch: toyos_build::arch::Arch, name: &'static str) -> (String, Vec<u8>) {
    type Built = Vec<(toyos_build::arch::Arch, &'static str, Vec<u8>)>;
    static BUILT: std::sync::Mutex<Built> = std::sync::Mutex::new(Vec::new());
    let mut built = BUILT.lock().expect("no build panics holding this");
    let bytes = match built.iter().find(|(a, n, _)| *a == arch && *n == name) {
        Some((.., bytes)) => bytes.clone(),
        None => {
            let bytes = qemu::build_toyos_bin(arch, &compile::repo_root().join("tests/guest/rust-tests"), name);
            built.push((arch, name, bytes.clone()));
            bytes
        }
    };
    (format!("bin/test_rs_{name}"), bytes)
}

/// The same for its job `test_rs_random_draws`.
const VIRT_RANDOM: &str = "random_draws";

/// The same for its job `test_rs_wall_clock_now`.
const VIRT_WALL_CLOCK: &str = "wall_clock_now";

/// The instant `tests/images/virtjobcase.toml`'s RTC starts at, and the same in Unix
/// seconds: years from any host's own, so a reading on it came from the RTC.
const VIRT_RTC_BASE: &str = "2033-03-07T09:14:25";
const VIRT_RTC_BASE_SECS: u64 = 1_993_799_665;

/// What `wall_clock_now` says before the `SYS_CLOCK_EPOCH` its line carries.
const WALL_CLOCK_SAID: &str = "wall-clock: epoch=";

/// `tests/images/virtjobcase.toml`'s jobs, in the order its job list runs them, and what
/// each says once the kernel kept what it asks about.
const VIRT_JOBS: &[(&str, &str)] = &[
    ("preempt", "preempt: the counting thread was preempted twice"),
    ("fp_isolation", "fp_isolation: v0-v31, FPCR and FPSR survived"),
    ("first_entry", "first_entry: x1-x30 were zero"),
    ("unmap_touch", UNMAP_TOUCH_SAID),
    ("debug_refused", "debug_refused: SYS_DEBUG's double fault and TLB acknowledgement delay were refused"),
    ("test_rs_ring0_timer_in_syscall", "the timer interrupted the syscall's body and re-armed a quantum"),
    ("test_rs_random_draws", RANDOM_DRAWS_SAID),
    ("test_rs_wall_clock_now", WALL_CLOCK_SAID),
    ("test_rs_abuse_readonly_copyout", "a syscall writes only where its caller could store"),
];

/// What `random_draws` says before the draw its line carries.
const RANDOM_DRAWS_SAID: &str = "random_draws: 8 threads drew 1000 times each and no two draws were alike; one more: ";

/// What the kernel says once the loader's seed is in its generator's key.
const LOADER_SEED_MIXED: &str = "random: the loader's seed is mixed into the generator's key";

/// Boot `tests/images/virtjobcase.toml` on one CPU, because `preempt` and `fp_isolation`
/// see a sibling run only when it took theirs, with its RTC at
/// [`VIRT_RTC_BASE`]. The kernel carries `SYS_DEBUG` for `debug_refused`, and
/// every job runs in every boot of the case.
fn boot_virt_jobs(profile: qemu::Profile) -> QemuInstance {
    let config = compile::repo_root().join("tests/images/virtjobcase.toml");
    QemuInstance::boot_with_options(
        &config,
        &[],
        &[],
        BootOptions {
            profile,
            smp: 1,
            kernel_features: toyos_build::build::TEST_KERNEL,
            ready_marker: "control registers: SCTLR_EL1=",
            extra_root_files: [VIRT_COPYOUT, VIRT_RING0_TIMER, VIRT_RANDOM, VIRT_WALL_CLOCK]
                .map(|name| suite_bin(profile.arch(), name))
                .to_vec(),
            rtc_base: Some(VIRT_RTC_BASE),
            ..Default::default()
        },
    )
}

/// Boot [`boot_virt_jobs`]' case and judge its job `job`: it ends with exit 0,
/// having said what [`VIRT_JOBS`] has it say.
fn virt_job(profile: qemu::Profile, job: &str) -> Result<(), String> {
    let (_, said) = VIRT_JOBS.iter().find(|(name, _)| *name == job).expect("a job of the case");
    let mut qemu = boot_virt_jobs(profile);
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
    "acpi: an unlisted register was read and written, an unlisted address below 1 MiB was refused RegisterCached both ways, and the interrupt controllers, the HPET and a function's BAR DeviceMemory",
    "acpi: the FACS read through as type 10, its write was refused FacsWrite, and the memory after it was written and put back",
    "acpi: COM1, the CMOS index, the 8259 and the configuration mechanism were refused KernelPort; the i8042's row ClaimedPort; PM1a_CNT read and refused its write ReadOnlyPort; the POST port was written",
    "acpi: ACPI_ENABLE and ACPI_DISABLE were refused SMI_CMD as KernelCommand and SCI_EN stayed set, a write wider than a byte was refused CommandSpan, every firmware call made was read back from the port and counted on the boot processor, and a storm of them was refused CommandRate",
    "by its address and through ECAM; every header and the LPC bridge's PMBASE were refused AddressMap, the NVMe controller its driver holds HeldFunction, and the host bridge's 0x44 was written",
    "acpi: the lock a dead holder left taken read free; it was taken and given back, given back with GBL_RLS where the firmware had asked, and found pending while the firmware owned it",
    "acpi: a sleep type wider than three bits was refused InvalidArgument, the next holder's replaced a dead one's, and a second under one claim was refused AlreadyExists",
];

/// Boot `tests/images/acpicase.toml`, whose one job is `test_rs_acpi_mediated`, on the
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
    let case = compile::repo_root().join("tests/images/acpicase.toml");
    let mut qemu = QemuInstance::boot_with_options(
        &case,
        &[],
        &[],
        BootOptions {
            // The test kernel, for the Global Lock's actuator.
            kernel_features: ACTUATOR_KERNEL,
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

/// Boot `tests/images/acpicase.toml` on the shipping kernel with `acpi_mediated`'s
/// `outlived` arm staged: a holder supplies q35's own sleep type and exits,
/// its claim and one more that supplied nothing are released, and the probe,
/// holding no claim, asks for the power-off. The kernel kept one sleep type,
/// and QEMU's ICH9 stops the guest on it: what a holder supplied stands once
/// the holder is gone. A probe that ends instead was refused its power-off.
fn acpi_supply_outlives_holder() -> Result<(), String> {
    const JOB: &str = "test_rs_acpi_mediated";
    const ASKED: &str = "acpi: asking for the power-off with no claim held, on what a holder that is gone supplied";
    let case = compile::repo_root().join("tests/images/acpicase.toml");
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

/// What `acpi_mediated`'s `press` arm says once the fixed power button alone
/// is enabled and the SCI acknowledged, which is when the button is pressed.
const PRESS_ARMED: &str = "acpi: the fixed power button is enabled alone and the SCI acknowledged";

/// What it says once the press was served.
const PRESS_SERVED: &str = "acpi: the press came as one SCI with its status latched, and no second while it was unserved";

/// Boot `tests/images/acpicase.toml` on the shipping kernel with `acpi_mediated`'s
/// `press` arm staged: the probe holds the `acpi` claim, and QEMU presses the
/// power button once the probe says it is armed. The press reaches the probe
/// as one SCI, and the kernel holds the line masked until the probe has
/// served it; the probe's verdict is its own exit and its last line.
fn acpi_press_is_one_sci() -> Result<(), String> {
    const JOB: &str = "test_rs_acpi_mediated";
    let case = compile::repo_root().join("tests/images/acpicase.toml");
    let mut qemu = QemuInstance::boot_with_options(
        &case,
        &[],
        &[],
        BootOptions {
            ready_marker: "acpi: the ACPI row: ",
            extra_root_files: vec![
                suite_bin(toyos_build::arch::Arch::X86_64, "acpi_mediated"),
                // The arm's name is the file's: `acpi_mediated` asks whether it is there.
                ("share/acpi_mediated_press".to_string(), b"press\n".to_vec()),
            ],
            qmp: true,
            ..Default::default()
        },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_marker(&mut qemu, &mut console, PRESS_ARMED, "the probe to arm the power button")?;
    qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET)).power_button();
    let ended = format!("===TEST_END {JOB} ");
    await_guest(&mut qemu, &mut console, "the probe to serve the press", |said| said.contains(&ended))?;
    let said = serial::Serial::named("the pressed boot", console);
    said.must_be_clean()?;
    said.must_say(&format!("{ended}exit=0"))?;
    eprintln!("  [acpi] {}", said.must_say_after(PRESS_ARMED, PRESS_SERVED)?.trim());
    Ok(())
}

/// `osbench identity`, `osbench fair` and `osbench energy` on
/// tests/images/benchcase.toml with only those jobs: each exits 0, which it
/// does only once every row it ran measured or said why not, its capability
/// rows among them. A guest reads no package energy, so `energy` says why.
fn osbench_capability_groups_exit_0() -> Result<(), String> {
    const JOBS: [&str; 3] = ["osbench_identity", "osbench_fair", "osbench_energy"];
    let committed = compile::repo_root().join("tests/images/benchcase.toml");
    let text = fs::read_to_string(&committed).map_err(|e| format!("{}: {e}", committed.display()))?;
    let derived = toyos_metal::image::derive(&text, 300_000, &JOBS, &[]).map_err(|why| why.to_string())?;
    let config = common::lane::dir().join("osbench-capability-groups.toml");
    fs::write(&config, derived).map_err(|e| format!("{}: {e}", config.display()))?;
    let mut qemu = QemuInstance::boot_with_options(
        &config,
        &[],
        &[],
        BootOptions { ready_marker: bootlog::COMPLETE, ..Default::default() },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    let last = format!("===TEST_END {} ", JOBS[JOBS.len() - 1]);
    await_guest(&mut qemu, &mut console, "osbench's groups to end", |said| said.contains(&last))?;
    let said = serial::Serial::named("osbench boot", console);
    for job in JOBS {
        said.must_say(&format!("===TEST_END {job} exit=0==="))?;
    }
    Ok(())
}

/// Boot the image `--console-boot` builds, on the machine shape with a panel
/// for `/system/bin/console` to claim, and wait for what that image owes at
/// boot: the console up on its panel, having read the log and the keyboard,
/// and ps2server's i8042 armed, its keyboard and QEMU's PS/2 mouse both.
/// Nothing is typed at its shell, whose only input is the i8042:
/// `issues/the-console-loses-typed-keystrokes-under-host-load.md`.
fn console_image_boots() -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        &compile::repo_root().join("images/console.toml"),
        &[],
        &[],
        BootOptions { profile: qemu::Profile::Metal, ready_marker: bootlog::COMPLETE, ..Default::default() },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_marker(&mut qemu, &mut console, "console: ready ", "the console to take its panel")?;
    await_marker(&mut qemu, &mut console, ps2server::ARMED_BOTH, "ps2server to arm the i8042")?;
    let said = serial::Serial::named("the console image's boot", console);
    eprintln!("  [ps2server] {}", said.must_say(ps2server::KEYBOARD_RESET)?.trim());
    said.must_be_clean()
}

/// Eight spawns from each of a file the root server holds in memory, a copy
/// on DATA that the spawn reads whole, and a copy in the kernel's `/tmp`:
/// each child maps its parent's log ring, and once they have ended the census
/// is back to its first reading (`spawn_returns_the_census`). A census per kind and not the memory in use:
/// a served image is a region over pages the server keeps, so one kept past
/// its child costs no page.
fn a_spawn_from_an_image_frees_it_when_the_child_ends(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "spawn_returns_the_census";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let options = BootOptions { kernel_features: ACTUATOR_KERNEL, ..BootOptions::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], options);
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_spawn_returns_the_census", Duration::from_secs(120));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    for source in ["the served /system", "a copy in /state", "a copy in /tmp"] {
        let claim = format!("8 spawns from {source}: the census came back to its first reading");
        if !result.stdout.contains(&claim) {
            return Err(format!("the job never said {claim:?}:\n{}", result.stdout));
        }
    }
    Ok(())
}

/// What a boot whose marked slot's supervisor is bent says: the loader refused
/// that slot by the section, and the kernel was booted from the other.
fn bent_supervisor_fell_back(loader: &serial::Serial, kernel: &serial::Serial) -> Result<(), String> {
    let refused = toyos_update::policy::Refusal::Hash(toyos_update::image::SUPERVISOR);
    loader.must_say(&format!("Slot A: REFUSED, {refused}"))?;
    // `kernel/src/params.rs`'s `SLOT_RECORD`, which the kernel's binary keeps.
    kernel.must_say(&format!("boot: slot B, because the marked slot A was refused: {}", refused.word()))?;
    Ok(())
}

/// [`bent_supervisor_fell_back`] on a bench, whose loader keeps its own log.
const BENT_SUPERVISOR: &[metal::Arm] = &[metal::Arm {
    bent: Some(toyos_update::slots::SUPERVISOR_FILE),
    ..metal::once("bentsupervisor", "tests/images/testcases.toml", &[], &[])
}];

/// Boot `tests/images/testcases.toml` from a USB stick, as a metal boot does,
/// write the stick through usbd ([`metal::STICK_LOAD_JOB`]), press the power
/// button while it writes, and read the stick's log volume back with the metal
/// judge's verdict: this boot's `logkeeper` files hold `Boot: complete` and the
/// supervisor's stop line, so the log went through usbd's disk to the stick and
/// was made durable before the power went. Then usbd handed the stick back
/// with a port reset and left its controller reset, before the kernel's stop
/// ([`usb_stopped_before_the_reset`]).
fn the_log_lands_on_the_boot_stick(test_config: &Path) -> Result<(), String> {
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        extra_root_files: vec![suite_bin(toyos_build::arch::Arch::X86_64, "stick_load")],
        ..Default::default()
    };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let mut console = qemu.boot_log().to_string();
    qemu::await_marker(&mut qemu, &mut console, ": serving it as disk 0", "usbd to serve the boot stick")?;
    qemu::await_marker(&mut qemu, &mut console, toyos_logstream::LOG_OPENED, "logkeeper to open the log")?;
    // The stop is asked for under load: the stick is being written when the
    // power button is pressed, as the metal row's deadline finds it.
    writeln!(qemu.stdin_mut(), "run {}", metal::STICK_LOAD_JOB).map_err(|e| e.to_string())?;
    qemu.flush_stdin();
    // Any line past the first 1500, not one exact line: logkeeper sheds its
    // oldest console lines when the console falls behind the load.
    let from = console.len();
    qemu::await_guest(&mut qemu, &mut console, "the load to write the stick", |log| {
        log[from..].lines().any(|l| stick_load_line(l).is_some_and(|n| n >= 1500))
    })?;
    qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET)).power_button();
    let by = qemu.budget(Duration::from_secs(60));
    console.push_str(&qemu.await_exit(by)?);
    let mut stick = std::fs::File::open(qemu.boot_image()).map_err(|e| e.to_string())?;
    let files = toyos_build::image::log_volume_files(&mut stick)?;
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    let listing = names.join("\n");
    let (_, ours) = bootlog::split_listing(&listing);
    let log: String = files
        .iter()
        .filter(|(name, _)| ours.contains(&name.as_str()))
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        .collect();
    let reached = metal::bytes_of_lines(&log, "stick_load: line ");
    if reached < 1 << 20 {
        return Err(format!("{reached} bytes of the load's lines are on the stick's log volume, under a mebibyte"));
    }
    if let Err(unfit) = bootlog::verdict(&log) {
        return Err(format!(
            "the stick's log volume ({names:?}) is no whole boot's log: {unfit:?}\n--- its log:\n{log}\n--- the console:\n{console}"
        ));
    }
    eprintln!("  [log] {} file(s) on the stick, {} bytes of this boot's log", names.len(), log.len());
    usb_stopped_before_the_reset(&log, &console)
}

fn stick_load_line(line: &str) -> Option<u64> {
    let (_, rest) = line.split_once("stick_load: line ")?;
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..digits].parse().ok()
}

/// usbd ended for the stop with every connected port's reset finished and its
/// controller halted and reset (its exit code, which the kernel records,
/// because its own line may reach no log), and it ended before the kernel's
/// stop: the supervisor waited for it.
fn usb_stopped_before_the_reset(log: &str, console: &str) -> Result<(), String> {
    // On the boot's one clock: the request is in the file (its line can be
    // one the console dropped under the load), and what follows the flush
    // is the kernel's, on the console.
    use toyos_logstream::Source;
    let at = |text: &str, from: Source, opens: &str| {
        text.lines()
            .find_map(|l| toyos_logstream::parse(l).filter(|p| p.source == from && p.text.starts_with(opens)))
            .map(|p| (p.ms, p.text.to_string()))
    };
    let asked = at(log, Source::Program("supervisor"), "supervisor: power: the machine stops");
    let ended = at(console, Source::Kernel, "exit: usbd pid=");
    let stopped = at(console, Source::Kernel, toyos_quiesce::STOPPED);
    let (Some((Some(asked), _)), Some((Some(ended), line)), Some((Some(stopped), _))) = (asked, ended, stopped) else {
        return Err(format!("no stop request in the file, or no usbd end and kernel stop on the console\n{console}"));
    };
    if !(asked <= ended && ended <= stopped) {
        return Err(format!("usbd ended at {ended} ms, not between the stop's request ({asked} ms) and the kernel's stop ({stopped} ms)"));
    }
    if !line.contains(" code=0 ") {
        return Err(format!("usbd's stop left a port in reset or its controller running: {line}"));
    }
    eprintln!("  [stop] {}", line.trim());
    Ok(())
}

/// Boot `images/console.toml` with a `usb-kbd` and type `ls` and Enter at it
/// through QMP, which sends every key to that keyboard: the console's shell
/// runs it, so usbd bound the keyboard, read its reports and gave the owner
/// of input its keys.
fn a_usb_key_reaches_the_console() -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        &compile::repo_root().join("images/console.toml"),
        &[],
        &[],
        BootOptions { profile: qemu::Profile::MetalUsbKeyboard, qmp: true, ready_marker: bootlog::COMPLETE, ..Default::default() },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_marker(&mut qemu, &mut console, "console: ready ", "the console to take its panel")?;
    await_marker(&mut qemu, &mut console, "collection 0: a Keyboard", "usbd to bind the keyboard")?;
    {
        let mut input = qemu::QmpInput::open(qemu.qmp_socket());
        for key in ["l", "s", "ret"] {
            input.keys(&[(key, true)]);
            input.keys(&[(key, false)]);
        }
    }
    await_guest(&mut qemu, &mut console, "the shell to run what was typed", |c| {
        // A bare `ls` is the shell's helper, spawned by the name it was typed as.
        c.lines().any(|l| l.contains("spawn: /system/bin/ls "))
    })?;
    serial::Serial::named("the typed-on console boot", console).must_be_clean()
}

/// Boot `tests/images/panelcase.toml` on [`qemu::Profile::MetalNoX2apic`]'s CPUs, which
/// offer no x2APIC, and wait for its compositor to hold the panel: every CPU
/// declared its local APIC in xAPIC mode, every CPU the firmware named came
/// up — started by an xAPIC ICR's two halves — and the desktop ran across
/// them, which every timer re-arm, EOI and TLB shootdown on the way reached
/// through the xAPIC's MMIO page.
fn desktop_without_x2apic() -> Result<(), String> {
    const CPUS: u32 = 4;
    let mut qemu = QemuInstance::boot_with_options(
        &compile::repo_root().join("tests/images/panelcase.toml"),
        &[],
        &[],
        BootOptions {
            profile: qemu::Profile::MetalNoX2apic,
            smp: CPUS,
            qmp: true,
            ready_marker: bootlog::COMPLETE,
            ..Default::default()
        },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_marker(&mut qemu, &mut console, "compositor: ready", "the compositor to take its panel")?;
    let taken = qemu.screendump_while(Duration::from_secs(30), Duration::from_millis(200), |d| d.fill() != FILL_BOOT);
    if taken.fill() == FILL_BOOT {
        return Err(format!("the compositor said it is ready and never took the screen\n{console}"));
    }
    let said = serial::Serial::named("the boot without x2APIC", console);
    said.must_be_clean()?;
    apic_mode_held(&said, CPUS, "Xapic")?;
    smp_roster_and_tsc_trail(said.text(), CPUS)?;
    eprintln!("  [xapic] {CPUS} CPUs in xAPIC mode, and the compositor holds the panel");
    Ok(())
}

/// Every one of `cpus` CPUs declared its local APIC in `mode`, as
/// `control_regs` names it, and the machine's report says the same.
fn apic_mode_held(log: &serial::Serial, cpus: u32, mode: &str) -> Result<(), String> {
    for cpu in 0..cpus {
        let line = log.must_say(&format!("control_regs: cpu{cpu} apic_base="))?;
        if !line.trim_end().ends_with(&format!(" {mode}")) {
            return Err(format!("cpu{cpu} did not declare its local APIC {mode}: {line:?}"));
        }
    }
    let held = log.must_say(&format!("control_regs: {cpus} of {cpus} cpus hold "))?;
    if !held.trim_end().ends_with(&format!(" apic={mode}")) {
        return Err(format!("the machine's declaration is not {mode}: {held:?}"));
    }
    log.must_say(&format!("LAPIC: {mode} enabled ("))?;
    Ok(())
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

/// One boot of the job case entered at EL2, every job judged: the kernel's
/// entry there writes `CNTHCTL_EL2`, `CNTVOFF_EL2` and `CPTR_EL2` for the
/// timer and FP the jobs then use at EL0, which a boot under HVF never runs.
fn virt_jobs_at_el2(profile: qemu::Profile) -> Result<(), String> {
    let mut qemu = boot_virt_jobs(profile);
    let mut serial = virt_console(&qemu);
    const ENTERED: &str = "as declared; entered at EL2";
    if !serial.contains(ENTERED) {
        return Err(format!("{ENTERED:?} not on the PL011, so this boot judges no EL2 entry\nserial:\n{serial}"));
    }
    for (job, said) in VIRT_JOBS {
        judge_virt_job(&mut qemu, &mut serial, job, said)?;
    }
    Ok(())
}

/// Two boots of the job case, each keyed from the loader's seed, and the draw
/// `random_draws` prints on each: the two differ. A generator keyed from
/// anything an image carries prints one draw on every boot.
fn virt_random_differs(profile: qemu::Profile) -> Result<(), String> {
    let mut draws = Vec::new();
    for boot in 1..=2 {
        let mut qemu = boot_virt_jobs(profile);
        let mut serial = virt_console(&qemu);
        judge_virt_job(&mut qemu, &mut serial, "test_rs_random_draws", RANDOM_DRAWS_SAID)?;
        if !serial.contains(LOADER_SEED_MIXED) {
            return Err(format!("{LOADER_SEED_MIXED:?} not on the PL011 of boot {boot}\nserial:\n{serial}"));
        }
        let draw = serial
            .lines()
            .find_map(|l| l.split_once(RANDOM_DRAWS_SAID).map(|(_, draw)| draw.trim().to_string()))
            .expect("judge_virt_job found the line");
        if draw.len() != 64 || !draw.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("boot {boot} printed no 32-byte draw: {draw:?}"));
        }
        draws.push(draw);
    }
    if draws[0] == draws[1] {
        return Err("two boots of one image printed the same 32-byte draw".to_string());
    }
    eprintln!("  [virt] two boots keyed from the loader's seed printed two draws");
    Ok(())
}

/// The job case's `wall_clock_now`: what `SYS_CLOCK_EPOCH` and std's
/// `SystemTime` answer is the instant staged in the RTC, which the loader read
/// through firmware's `GetTime`. Not before the instant, and not after where
/// the RTC, running at the host's pace, can have got to by the time the job's
/// end was read, counted from before the boot was built: both ends are
/// causality, and a clock with no reading, or one read elsewhere, is out by
/// years.
fn virt_wall_clock_utc(profile: qemu::Profile) -> Result<(), String> {
    let launched = std::time::Instant::now();
    let mut qemu = boot_virt_jobs(profile);
    let mut serial = virt_console(&qemu);
    judge_virt_job(&mut qemu, &mut serial, "test_rs_wall_clock_now", WALL_CLOCK_SAID)?;
    let lived = launched.elapsed().as_secs().saturating_add(1);
    let on_the_base = VIRT_RTC_BASE_SECS..=VIRT_RTC_BASE_SECS + lived;
    for said in [WALL_CLOCK_SAID, "wall-clock: std_epoch="] {
        let read = serial
            .lines()
            .find_map(|l| l.split_once(said))
            .and_then(|(_, rest)| rest.split_whitespace().next()?.parse::<u64>().ok());
        match read {
            Some(secs) if on_the_base.contains(&secs) => {
                eprintln!("  [virt] {said}{secs}, {}s past the staged {VIRT_RTC_BASE}", secs - VIRT_RTC_BASE_SECS)
            }
            _ => {
                return Err(format!(
                    "{said} read {read:?}, outside {on_the_base:?}, {VIRT_RTC_BASE} and the {lived}s since the \
                     launch\nserial:\n{serial}"
                ))
            }
        }
    }
    Ok(())
}

/// A machine with no source at all: no virtio-rng, so firmware has no
/// `EFI_RNG_PROTOCOL` and the loader hands no seed, and a CPU with no RNDR.
/// The kernel says each and refuses by name before its first hash container.
fn virt_no_seed_refused(profile: qemu::Profile, test_config: &Path) -> Result<(), String> {
    const REFUSED: &str = "random: nothing keyed the generator";
    let options = BootOptions { profile, ready_marker: REFUSED, ..Default::default() };
    // The premise: the machine the test names is the one QEMU is asked for.
    let argv = qemu::profile_argv(&options);
    if let Some(rng) = argv.iter().find(|a| a.contains("virtio-rng")) {
        return Err(format!("the machine with no seed has {rng}: {argv:?}"));
    }
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let rest = qemu.drain_until(Duration::from_secs(6), |l| l.contains("EARLY PANIC: panicked at"));
    let serial = format!("{}\n{rest}", qemu.boot_log());
    for want in [
        "random: the loader's seed is not mixed: the loader handed none",
        "random: RNDR is not mixed: ID_AA64ISAR0_EL1.RNDR is zero, so this CPU has no RNDR",
        REFUSED,
        "EARLY PANIC: panicked at",
    ] {
        if !serial.contains(want) {
            return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
        }
    }
    for never in ["random: the generator is keyed", "paging: the direct map holds memory below"] {
        if serial.contains(never) {
            return Err(format!("{never:?} on the PL011 of a machine with no source\nserial:\n{serial}"));
        }
    }
    Ok(())
}

/// A machine whose MCFG decodes sixteen buses, with RAM past them: the kernel
/// maps those sixteen buses' megabytes and no more, and enumerates the
/// functions QEMU put there and nothing else, and the boot goes on.
fn virt_low_ecam(profile: qemu::Profile, test_config: &Path) -> Result<(), String> {
    const WINDOW: &str = "ACPI: ECAM window: segment 0 buses 0x00..=0x0f, bus 0 at 0x3f000000";
    const MAPPED: &str = "mmio: 0x3f000000+0x1000000 ";
    const SPAWNED: &str = "spawn: /system/bin/logkeeper pid=";
    /// What QEMU puts on bus 0 of this profile: the host bridge, the xHCI and
    /// the virtio-rng, as `vendor=… device=…`.
    const FUNCTIONS: &[&str] = &[
        "00:00.0 [0600] vendor=1b36 device=0008",
        "00:01.0 [0c03] vendor=1033 device=0194",
        "00:02.0 [00ff] vendor=1af4 device=1005",
    ];
    let options = BootOptions { profile, ready_marker: "control registers: SCTLR_EL1=", ..Default::default() };
    let argv = qemu::profile_argv(&options);
    if !argv.iter().any(|a| a.contains("highmem-ecam=off")) {
        return Err(format!("the machine keeps its ECAM high: {argv:?}"));
    }
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let rest = qemu.drain_until(Duration::from_secs(30), |l| l.contains(SPAWNED));
    let serial = format!("{}\n{rest}", qemu.boot_log());
    // What was read and what was mapped before what was said about it.
    pci_inventory(&serial).map_err(|why| format!("{why}\nserial:\n{serial}"))?;
    let rows: Vec<&str> = serial.lines().filter_map(|l| l.split("  PCI ").nth(1)).collect();
    let found: Vec<&str> = rows.iter().map(|row| row.split(" prog_if=").next().unwrap_or(row)).collect();
    if found != FUNCTIONS {
        return Err(format!("the kernel enumerated {found:#?}, and QEMU put {FUNCTIONS:#?} on the window's buses"));
    }
    if let Some(wider) = serial.lines().find(|l| l.contains("mmio: 0x3f000000+") && !l.contains(MAPPED)) {
        return Err(format!("the window was mapped wider than its buses: {wider:?}\nserial:\n{serial}"));
    }
    for want in [MAPPED, WINDOW, SPAWNED] {
        if !serial.contains(want) {
            return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
        }
    }
    eprintln!("  [virt] {WINDOW}; {} functions: {found:?}", found.len());
    Ok(())
}

/// The SMMUv3 armed from the IORT, judged by what two of QEMU's
/// `iommu-testdev` can and cannot write through it
/// (`kernel/src/arch/aarch64/smmu/selftest.rs`): `GBPA` read back aborting,
/// a `CMD_SYNC` consumed; one device's write refused on the entry its stream
/// starts with and nothing recorded, and landing where its own domain maps
/// it; the other's three, under a StreamID no function is routed from inside
/// the stream table, refused, the first two recorded as no function's and the
/// third counted and not written, and the machine going on; and the first's again once its domain takes that address back,
/// refused, recorded as the kernel's, and the machine halted on it. Each
/// record taken on the wired SPI and named. Read to the halt path's own line
/// past the stop, so nothing the guest could still say is missed.
fn virt_smmu(profile: qemu::Profile, test_config: &Path) -> Result<(), String> {
    const FAULT: &str = "iommu: DMA FAULT owner=";
    const WENT_ON: &str = "smmu-selftest: the unrouted function's three events were read, and the machine goes on";
    // `panic_reboot::arm`'s line, which `halt_all_cpus` writes once every
    // other CPU is stopped: one of its two heads.
    let halted = |l: &str| l.contains("panic: rebooting") || l.contains("panic: holding this panel");
    let options = BootOptions {
        profile,
        kernel_params: &["smmu-selftest"],
        ready_marker: "control registers: SCTLR_EL1=",
        ..Default::default()
    };
    let argv = qemu::profile_argv(&options);
    for want in ["iommu=smmuv3", "iommu-testdev"] {
        if !argv.iter().any(|a| a.contains(want)) {
            return Err(format!("the machine has no {want}: {argv:?}"));
        }
    }
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let rest = qemu.drain_until(Duration::from_secs(30), halted);
    let serial = format!("{}\n{rest}", qemu.boot_log());
    let lines: Vec<&str> = serial.lines().collect();
    let at = |want: &str| lines.iter().position(|l| l.contains(want));
    let line = |want: &str| at(want).map(|i| lines[i].to_string());
    let hex = |text: &str| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok();
    if let Some(failed) = line("smmu-selftest: FAIL") {
        return Err(format!("{failed}\nserial:\n{serial}"));
    }
    let Some(stop) = lines.iter().position(|l| halted(l)) else {
        return Err(format!("the machine never reached the halt path's line past the stop\nserial:\n{serial}"));
    };
    let Some(armed) = line("armed, CR0ACK") else {
        return Err(format!("the SMMUv3 was never armed\nserial:\n{serial}"));
    };
    for want in ["every other entry invalid", "a CMD_SYNC consumed", "events on SPI 106"] {
        if !armed.contains(want) {
            return Err(format!("{want:?} not in {armed:?}"));
        }
    }
    eprintln!("  [virt] {armed}");
    let table = armed.split_once("in a table of ").and_then(|(_, rest)| rest.split(',').next()?.parse::<u64>().ok());
    let Some(table) = table else {
        return Err(format!("{armed:?} names no table size"));
    };
    // `SMMU_GBPA.ABORT`, bit 20, as the unit reads it back.
    let gbpa = line(", every transaction aborts while SMMUEN is clear")
        .and_then(|l| l.split_once(": GBPA ").and_then(|(_, v)| hex(v.split(',').next()?)));
    match gbpa {
        Some(gbpa) if gbpa & 1 << 20 != 0 => eprintln!("  [virt] GBPA {gbpa:#x}: ABORT"),
        _ => return Err(format!("GBPA does not read back aborting: {gbpa:?}\nserial:\n{serial}")),
    }
    let Some(unrouted) = line("is given no route") else {
        return Err(format!("the selftest left no function unrouted\nserial:\n{serial}"));
    };
    eprintln!("  [virt] {unrouted}");
    let Some(stream) = unrouted.split_once(", StreamID ").and_then(|(_, rest)| hex(rest.split(',').next()?)) else {
        return Err(format!("{unrouted:?} names no StreamID"));
    };
    if stream >= table {
        return Err(format!("StreamID {stream:#x} is past a table of {table}: {unrouted:?}"));
    }
    for (said, verdict) in [
        ("on the entry its stream starts with", "refused"),
        ("mapped to", "landed there"),
        ("'s write 1 at ", "refused"),
        ("'s write 2 at ", "refused"),
        ("'s write 3 at ", "refused"),
        ("which its domain no longer maps", "refused"),
    ] {
        let Some(found) = line(said) else {
            return Err(format!("the selftest never said {said:?}\nserial:\n{serial}"));
        };
        eprintln!("  [virt] {found}");
        if !found.ends_with(verdict) {
            return Err(format!("{found}\nserial:\n{serial}"));
        }
    }
    let Some(again) = line("which its domain no longer maps") else {
        return Err(format!("the selftest never wrote where its domain took a mapping back\nserial:\n{serial}"));
    };
    let aimed = again.split_once("smmu-selftest: ").and_then(|(_, said)| said.split_once("'s write at "));
    let Some((function, Some(address))) =
        aimed.map(|(function, rest)| (function, rest.split(' ').next().and_then(hex)))
    else {
        return Err(format!("{again:?} names no function and address"));
    };
    let faults: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].contains(FAULT)).collect();
    for &fault in &faults {
        eprintln!("  [virt] {}", lines[fault]);
    }
    // The unit's first four events, so the write on the entry its stream
    // starts with was not recorded; the stray's third is counted and, not
    // being a power of two, not written.
    let wants = [
        (format!("owner=none unit0 stream={stream:#x} "), " unitfaults=1 streamfaults=1 ", "C_BAD_STE"),
        (format!("owner=none unit0 stream={stream:#x} "), " unitfaults=2 streamfaults=2 ", "C_BAD_STE"),
        (
            format!("owner=kernel unit0 stream={function} addr={address:#018x} access=write"),
            " unitfaults=4 ",
            "F_TRANSLATION",
        ),
    ];
    if faults.len() != wants.len() {
        return Err(format!("{} events reached the handler, not {}\nserial:\n{serial}", faults.len(), wants.len()));
    }
    for (&fault, (named, count, name)) in faults.iter().zip(&wants) {
        let fault = lines[fault];
        if !fault.contains(named.as_str()) || !fault.contains(count) || !fault.ends_with(name) {
            return Err(format!("the event does not name {named:?},{count}and {name}: {fault}"));
        }
    }
    // The machine went on past the unrouted records and stopped at the
    // kernel's: the selftest's line between them, and the stop after the last.
    let went_on = at(WENT_ON).ok_or_else(|| format!("the selftest never said {WENT_ON:?}\nserial:\n{serial}"))?;
    if !(faults[1] < went_on && went_on < faults[2] && faults[2] < stop) {
        return Err(format!(
            "the unrouted records, the selftest going on, the kernel's record and the stop are out of order: \
             lines {}, {went_on}, {} and {stop}\nserial:\n{serial}",
            faults[1], faults[2]
        ));
    }
    // A halt, which writes the record and the stop's line and no panic.
    if let Some(death) = lines.iter().find(|l| serial::died(l).is_some() && !l.contains("owner=kernel")) {
        return Err(format!("the machine died of something other than the record: {death}\nserial:\n{serial}"));
    }
    Ok(())
}

/// `tests/guest/rust-tests`' binaries that `tests/images/virtclaimcase.toml` runs as its
/// jobs, each `test_rs_` and its name.
const VIRT_CLAIM_LPI: &str = "claim_lpi";
const VIRT_CLAIM_UNROUTED: &str = "claim_unrouted";
const VIRT_CLAIM_BAR_FAULT: &str = "claim_bar_fault";

/// What `claim_bar_fault` keeps in edu's two DMA address registers while its
/// children fault in the BAR (`MARK_SOURCE`, `MARK_DESTINATION` there): on
/// the console only if the kernel read through the BAR for a crash report.
const BAR_MARKS: [u64; 2] = [0xbad0_c0de_0000_5eed, 0xbad1_c0de_0000_5eed];

/// A claimed function's message, translated by the ITS into its function's
/// LPI, and the claims the kernel refuses beside it, on `tests/images/virtclaimcase.toml`.
/// Its first job claims QEMU's `edu` and reads every message it asks it for as
/// one interrupt of its claim (`tests/guest/rust-tests/src/bin/claim_lpi.rs`),
/// judged beside it on the kernel's lines: the ITS armed from the MADT; the
/// function handed over, its DeviceID the requester ID QEMU's IORT maps it
/// to, its LPI raised; and at the claim's end the DeviceID
/// unmapped again. The second is refused the `e1000e` the IORT routes past
/// the SMMUv3, by the kernel's name for it, and claims edu after it. The
/// third faults twice in edu's BAR, and names it as a syscall's buffer; no
/// crash report reads through it. And no access of a function's is refused
/// at the SMMUv3.
fn virt_claim_lpi(profile: qemu::Profile) -> Result<(), String> {
    const JOB: &str = "test_rs_claim_lpi";
    const SAID: &str = "claim_lpi: 16 messages edu sent were read as 16 interrupts of its claim, one each";
    const EDU: &str = "[1234:11e8]";
    let config = compile::repo_root().join("tests/images/virtclaimcase.toml");
    // One CPU, so each message is taken where the job wrote the register
    // that sends it: at EL0, which holds the preempt count at zero.
    let options = BootOptions {
        profile,
        smp: 1,
        ready_marker: "control registers: SCTLR_EL1=",
        extra_root_files: vec![
            suite_bin(profile.arch(), VIRT_CLAIM_LPI),
            suite_bin(profile.arch(), VIRT_CLAIM_UNROUTED),
            suite_bin(profile.arch(), VIRT_CLAIM_BAR_FAULT),
        ],
        ..Default::default()
    };
    let argv = qemu::profile_argv(&options);
    for want in ["msi=its", "iommu=smmuv3", "edu", "bypass_iommu=on", "e1000e"] {
        if !argv.iter().any(|a| a.contains(want)) {
            return Err(format!("the machine has no {want}: {argv:?}"));
        }
    }
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
    let mut serial = virt_console(&qemu);
    judge_virt_job(&mut qemu, &mut serial, JOB, SAID)?;
    // The release's own line, its last: the DeviceID's unmapping comes before it.
    await_marker(&mut qemu, &mut serial, &format!("{EDU} released; "), "the claim's release")?;
    let line = |want: &str| serial.lines().find(|l| l.contains(want)).map(str::to_string);
    let Some(armed) = line("ITS: 0 at 0x8080000 armed") else {
        return Err(format!("the MADT's ITS was never armed
serial:
{serial}"));
    };
    eprintln!("  [virt] {armed}");
    let Some(handed) = line(&format!("{EDU} handed over, ")) else {
        return Err(format!("edu was never handed over
serial:
{serial}"));
    };
    eprintln!("  [virt] {handed}");
    // `pcidev: PCI bb:dd.f [1234:11e8] handed over, LPI M`.
    let parsed = handed.split_once("pcidev: PCI ").and_then(|(_, rest)| {
        let (at, rest) = rest.split_once(' ')?;
        let lpi = rest.split_once("handed over, LPI ")?.1;
        Some((at.to_string(), lpi.trim().parse::<u32>().ok()?))
    });
    let Some((at, lpi)) = parsed else {
        return Err(format!("{handed:?} names no function and LPI"));
    };
    if lpi < 8192 {
        return Err(format!("{at}'s interrupt {lpi} is no LPI"));
    }
    // QEMU's IORT maps a function's requester ID to the SMMUv3's StreamID and
    // that to the ITS's DeviceID unchanged.
    let (bus, rest) = at.split_once(':').ok_or_else(|| format!("{at} is no bus:device.function"))?;
    let (dev, func) = rest.split_once('.').ok_or_else(|| format!("{at} is no bus:device.function"))?;
    let hex = |text: &str| u32::from_str_radix(text, 16).map_err(|e| format!("{text}: {e}"));
    let rid = hex(bus)? << 8 | hex(dev)? << 3 | hex(func)?;
    for want in [
        format!("ITS: {at}, DeviceID {rid:#x}, event 0 is LPI {lpi}"),
        format!("pcidev: PCI {at} took its first message on LPI {lpi}"),
        format!("pcidev: PCI {at} {EDU} released; "),
        format!("ITS: {at}, DeviceID {rid:#x}, unmapped from LPI {lpi}"),
    ] {
        let Some(found) = line(&want) else {
            return Err(format!("the kernel never said {want:?}
serial:
{serial}"));
        };
        eprintln!("  [virt] {found}");
    }

    judge_virt_job(
        &mut qemu,
        &mut serial,
        "test_rs_claim_unrouted",
        "claim_unrouted: e1000e, routed past the SMMUv3, was refused, and edu was claimed after",
    )?;
    await_kernel_exit(&mut qemu, &mut serial, "test_rs_claim_unrouted")?;
    // `pcidev::Refusal::NoMessage` of the ITS's `Refused::NoDeviceId`.
    const UNROUTED: &str = "NOT HANDED OVER — no message of its own could be made for it — the IORT gives it no \
                            DeviceID at this kernel's ITS behind the SMMUv3";
    let Some(refused) = serial.lines().find(|l| l.contains(UNROUTED)) else {
        return Err(format!("the kernel never refused e1000e by {UNROUTED:?}\nserial:\n{serial}"));
    };
    eprintln!("  [virt] {refused}");

    judge_virt_job(
        &mut qemu,
        &mut serial,
        "test_rs_claim_bar_fault",
        "claim_bar_fault: two faults in the BAR ended their processes, and a syscall naming it was refused",
    )?;
    await_kernel_exit(&mut qemu, &mut serial, "test_rs_claim_bar_fault")?;
    let faults: Vec<&str> = serial.lines().filter(|l| l.contains("FAULT pc=")).collect();
    if faults.len() != 2 {
        return Err(format!("{} user faults reported, not claim_bar_fault's two: {faults:?}\nserial:\n{serial}", faults.len()));
    }
    let console = serial.to_lowercase();
    for mark in BAR_MARKS {
        if console.contains(&format!("{mark:x}")) {
            return Err(format!("{mark:#x}, in edu's BAR, is on the console: a crash report read through it\nserial:\n{serial}"));
        }
    }
    if let Some(fault) = serial.lines().find(|l| l.contains("DMA FAULT")) {
        return Err(format!("the unit refused an access: {fault}\nserial:\n{serial}"));
    }
    Ok(())
}

/// Wait for the kernel's line of `job`'s own exit, its `code=0` telling it from
/// a child of the same name a fault ended: the kernel's lines can reach the
/// PL011 after test-runner's line of the job's end, so that line is no sign
/// the kernel has said all it will of the job.
fn await_kernel_exit(qemu: &mut QemuInstance, serial: &mut String, job: &str) -> Result<(), String> {
    let exit = format!("exit: {job} pid=");
    await_guest(qemu, serial, &format!("the kernel's line of {job}'s exit"), |log| {
        log.lines().any(|l| l.contains(&exit) && l.contains(" code=0 "))
    })
    .map_err(|why| format!("{why}\nserial:\n{serial}"))
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

/// The windows on AArch64: `tests/images/virtsmpcase.toml` on [`VIRT_CPUS`] CPUs of a
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

/// Boot `tests/images/virtsmpcase.toml` as `options` say.
fn boot_virt_smp(options: BootOptions) -> QemuInstance {
    let config = compile::repo_root().join("tests/images/virtsmpcase.toml");
    QemuInstance::boot_with_options(
        &config,
        &[],
        &[],
        BootOptions {
            ready_marker: "control registers: SCTLR_EL1=",
            extra_root_files: vec![
                suite_bin(options.profile.arch(), VIRT_COUNTERS_READ),
                suite_bin(options.profile.arch(), VIRT_TRACE_READ),
                suite_bin(options.profile.arch(), VIRT_TIMING),
            ],
            ..options
        },
    )
}

/// What `counters_read` says once every CPU answered for itself.
const COUNTERS_READ_SAID: &str = "counters_read: every cpu answered for itself, and each counter is a right's";

/// What `trace_read` says once the diary read back as the scheduler wrote it.
const TRACE_READ_SAID: &str = "trace_read: a wake precedes its pick, a cursor is its reader's own, and the diary is TRACE's";

/// What `timing_protection` says once `PSTATE.DIT`, which every emulated CPU
/// of the case has, followed each of its threads.
const TIMING_DIT_SAID: &str = "timing_protection: PSTATE.DIT followed ";

/// Boot `tests/images/virtsmpcase.toml` on [`VIRT_CPUS`] CPUs under `profile`, whose
/// firmware enters every CPU at EL`el` and whose FADT names PSCI's `conduit`:
/// each CPU is started by `CPU_ON`, holds the control-register declaration as
/// entered there and joins the scheduler, and the case's jobs `unmap_touch`,
/// `test_rs_counters_read`, `test_rs_trace_read` and `test_rs_timing_protection`
/// end with exit 0. Then its job `shutdown` stops the machine and powers it
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
    judge_virt_job(&mut qemu, &mut serial, "test_rs_timing_protection", TIMING_DIT_SAID)?;
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

/// `tests/images/virtrebootcase.toml`'s one job asks for a reboot: the boot's last word is
/// `Rebooting.`, QEMU stops for `guest-reset`, and its trace of PSCI holds one
/// `SYSTEM_RESET` and neither `CPU_OFF` nor `SYSTEM_OFF`. With `staged`
/// actuators `klogd` holds the console's wire as the stop begins, judged by
/// [`power::judge_the_held_wire`].
fn virt_reboot(profile: qemu::Profile, staged: &'static [&'static str]) -> Result<(), String> {
    let config = compile::repo_root().join("tests/images/virtrebootcase.toml");
    let trace = common::lane::dir().join("virt_reboot.psci");
    let mut qemu = QemuInstance::boot_with_options(
        &config,
        &[],
        &[],
        BootOptions {
            profile,
            qmp: true,
            psci_trace: Some(trace.clone()),
            kernel_params: staged,
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
    if !staged.is_empty() {
        power::judge_the_held_wire(&console, bootlog::REBOOTING, staged == power::wire_staged(true))?;
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
/// `tests/images/virtrebootcase.toml`'s job `reboot` is refused by name before anything is
/// torn down, ends with exit 1, and the boot never says `Rebooting.`.
fn virt_reboot_refused_without_psci(profile: qemu::Profile) -> Result<(), String> {
    let config = compile::repo_root().join("tests/images/virtrebootcase.toml");
    let mut qemu = QemuInstance::boot_with_options(
        &config,
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

/// `tests/images/virtpaniccase.toml` runs `test_rs_panic_halts_first` as its one job on
/// [`STOP_CPUS`] CPUs, and the halt SGI stops every CPU but the one going
/// fatal.
fn virt_fatal_halts_the_others_first(profile: qemu::Profile) -> Result<(), String> {
    let config = compile::repo_root().join("tests/images/virtpaniccase.toml");
    let fatal = qemu::build_toyos_bin(profile.arch(), &compile::repo_root().join("tests/guest/rust-tests"), "panic_halts_first");
    let qemu = QemuInstance::boot_with_options(
        &config,
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
        "virt_kernel_wx_leaf_is_refused" => faults::kernel_wx_leaf_is_refused(test_config, profile),
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
            // The port's stage 4: the generator keyed from the seed the loader
            // read from firmware's virtio-rng, the kernel's own tables, the GIC
            // and the timer, and a process at EL0 — the supervisor, whose every
            // page arrives by a demand fault and whose spawn of `logkeeper` is
            // a syscall the kernel answered.
            let options =
                BootOptions { profile, ready_marker: "control registers: SCTLR_EL1=", ..Default::default() };
            let argv = qemu::profile_argv(&options);
            if !argv.iter().any(|a| a.starts_with("virtio-rng-pci")) {
                return Err(format!("`virt` has no virtio-rng for firmware's EFI_RNG_PROTOCOL: {argv:?}"));
            }
            let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
            const SPAWNED: &str = "spawn: /system/bin/logkeeper pid=";
            let rest = qemu.drain_until(Duration::from_secs(30), |l| l.contains(SPAWNED));
            let serial = format!("{}\n{rest}", qemu.boot_log());
            for want in [
                LOADER_SEED_MIXED,
                "random: the generator is keyed from",
                "paging: the direct map holds memory below",
                "percpu: BSP cpu_id=0",
                "GIC: v",
                "clock: the generic timer counts at",
                "spawned the supervisor pid=",
                SPAWNED,
            ] {
                if !serial.contains(want) {
                    return Err(format!("{want:?} not on the PL011\nserial:\n{serial}"));
                }
            }
            Ok(())
        }
        "virt_timer_preempts" => virt_job(profile, "preempt"),
        "virt_fp_isolation" => virt_job(profile, "fp_isolation"),
        "virt_first_entry" => virt_job(profile, "first_entry"),
        "virt_unmap_touch" => virt_job(profile, "unmap_touch"),
        "virt_debug_refused" => virt_job(profile, "debug_refused"),
        "virt_readonly_copyout" => virt_job(profile, &format!("test_rs_{VIRT_COPYOUT}")),
        "virt_ring0_timer_in_syscall" => virt_job(profile, &format!("test_rs_{VIRT_RING0_TIMER}")),
        "virt_jobs_at_el2" => virt_jobs_at_el2(profile),
        "virt_random_differs" => virt_random_differs(profile),
        "virt_wall_clock_utc" => virt_wall_clock_utc(profile),
        "virt_no_seed_refused" => virt_no_seed_refused(profile, test_config),
        "virt_smmu" => virt_smmu(profile, test_config),
        "virt_claim_lpi" => virt_claim_lpi(profile),
        "virt_low_ecam" => virt_low_ecam(profile, test_config),
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
        "virt_reboot" => virt_reboot(profile, &[]),
        // The reboot with `klogd` holding the PL011's wire as the stop begins,
        // then keeping it through the stop's budget.
        "virt_reboot_wire_held" => virt_reboot(profile, power::wire_staged(false)),
        "virt_reboot_wire_kept" => virt_reboot(profile, power::wire_staged(true)),
        "virt_off_names_the_cpus_left_on" => virt_off_names_the_cpus_left_on(profile),
        "virt_reboot_refused_without_psci" => virt_reboot_refused_without_psci(profile),
        "screen_fatal_behind_a_painter" => {
            // The fatal halt with a painter holding the panel's latch and
            // never giving it back — which is what a painter is when the halt
            // IPI lands mid-paint. The actuator has the last boot checkpoint's
            // painter go fatal once it holds the latch, so the fatal path meets a
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
                    // The fatal path is the boot's own end: it never reaches userland.
                    ready_marker: HELD,
                    ..Default::default()
                },
            );
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
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images/panelcase.toml");
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
            // carry an `irq: cpu0` record, behind its stamp.
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

/// Run the machine-shape test, which owns its QEMU: the machine shape *is* the
/// test.
fn run_machine_test(name: &str, test_config: &Path) -> Result<(), String> {
    match name {
        "iommu_virtio_platform" => common::iommu::iommu_virtio_platform(),
        "a_virtio_function_without_access_platform_is_refused" => {
            common::iommu::a_virtio_function_without_access_platform_is_refused()
        }
        "the_uart_carries_the_log_again_once_the_console_carrier_goes" => {
            common::vmconsole::the_uart_carries_the_log_again_once_the_console_carrier_goes(test_config)
        }
        "amdvi_firmware_left" => common::iommu::amdvi_firmware_left(test_config),
        "vtd_firmware_left" => common::iommu::vtd_firmware_left(test_config),
        "amdvi_selftest" => common::iommu::amdvi_selftest(test_config),
        "nested_nmi_is_loud" => faults::nested_nmi_is_loud(test_config),
        "kernel_wx_leaf_is_refused" => faults::kernel_wx_leaf_is_refused(test_config, qemu::Profile::Metal),
        "a_stepped_tsc_is_slewed_then_demoted" => faults::a_stepped_tsc_is_slewed_then_demoted(test_config),
        "a_stalled_hpet_is_left_out" => faults::a_stalled_hpet_is_left_out(test_config),
        "acpi_press_is_one_sci" => acpi_press_is_one_sci(),
        "osbench_capability_groups_exit_0" => osbench_capability_groups_exit_0(),
        "acpi_mediated_access" => acpi_mediated_access(2),
        "acpi_lock_given_back_on_one_cpu" => acpi_mediated_access(1),
        "acpi_supply_outlives_holder" => acpi_supply_outlives_holder(),
        "machine_shutdown_short_stop" => power::machine_shutdown_short_stop(test_config),
        "machine_shutdown_wire_held" => power::machine_shutdown_wire_held(test_config, false),
        "machine_shutdown_wire_kept" => power::machine_shutdown_wire_held(test_config, true),
        "machine_shutdown_wire_at_the_seal" => power::machine_shutdown_wire_at_the_seal(test_config),
        "bar_map_again" => bar_map_again(test_config),
        "bar_map_behind_a_bridge" => bar_map_behind_a_bridge(test_config),
        "lent_device_memory_refused" => lent_device_memory_refused(test_config),
        "console_image_boots" => console_image_boots(),
        "a_spawn_from_an_image_frees_it_when_the_child_ends" => a_spawn_from_an_image_frees_it_when_the_child_ends(test_config),
        "a_bent_supervisor_is_refused_and_the_other_slot_boots" => {
            [qemu::Profile::Headless, qemu::Profile::Virt].into_iter().try_for_each(|profile| {
                let options = BootOptions { profile, bent: Some(toyos_update::slots::SUPERVISOR_FILE), ..Default::default() };
                let qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
                let loader = serial::Serial::named("loader's serial", qemu.loader_log());
                let kernel = serial::Serial::named("boot's console", qemu.boot_log().to_string());
                bent_supervisor_fell_back(&loader, &kernel)
            })
        }
        "the_log_lands_on_the_boot_stick" => the_log_lands_on_the_boot_stick(test_config),
        "a_usb_key_reaches_the_console" => a_usb_key_reaches_the_console(),
        "desktop_without_x2apic" => desktop_without_x2apic(),
        "nvme_disk_boots" => nvme_disk_boots(test_config),
        "a_host_disk_beside_the_stick_is_left_alone" => a_host_disk_beside_the_stick_is_left_alone(test_config),
        "the_data_partition_beside_the_stick_is_designated" => the_data_partition_beside_the_stick_is_designated(),
        "hosted_clang_hello" => common::hostedclang::hosted_clang_hello(),
        "libc_children" => common::libcchildren::libc_children(),
        "stream_holders" => stream_holders(test_config),
        "netstack_holders_exported_to_children" => netstack_holders_exported_to_children(),
        "udp_receive_times_out" => udp_receive_times_out(),
        "a_link_ends_under_a_held_stream_and_comes_back" => a_link_ends_under_a_held_stream_and_comes_back(),
        "a_client_killed_mid_connect_gives_its_place_back" => a_client_killed_mid_connect_gives_its_place_back(),
        "traffic_leaves_by_the_second_link_while_the_first_is_down" => traffic_leaves_by_the_second_link_while_the_first_is_down(),
        "session_lifecycle" => common::sessionlifecycle::session_lifecycle(),
        "a_launch_gives_each_unsent_stdio_slot_a_null" => common::launchstdio::a_launch_gives_each_unsent_stdio_slot_a_null(),
        "a_row_reaches_its_own_state_and_no_other" => common::stateview::a_row_reaches_its_own_state_and_no_other(),
        "usbd_drives_the_spare" => usbd_drives_the_spare(test_config),
        "usbd_refused_without_smi_filter" => usbd_refused_on_amdvi(
            test_config,
            qemu::Profile::HeadlessUsbSpareAmdVi,
            "has no SMI filter",
            claims::SMI_UNFILTERED,
        ),
        "usbd_refused_without_amdvi_remapping" => usbd_refused_on_amdvi(
            test_config,
            qemu::Profile::HeadlessUsbSpareAmdViNoRemap,
            "no message is remapped",
            claims::NOT_REMAPPED,
        ),
        "a_drives_row_holds_one_process_per_function" => a_drives_row_holds_one_process_per_function(),
        "hda_plays_to_the_controller_that_reaches_an_output" => {
            hda_plays_to_the_controller_that_reaches_an_output(test_config)
        }
        "vector_state_survives_a_switch" => vector_state_survives_a_switch(test_config),
        "std_threads_count_the_cpus_and_give_back_ended_threads" => std_threads_count_the_cpus_and_give_back_ended_threads(test_config),
        "kernel_objects_refused_alone" => kernel_objects_refused_alone(test_config),
        "memory_floor_holds_the_controls" => memory_floor_holds_the_controls(),
        "the_opening_exchange_with_qemus_agent_completes" => the_opening_exchange_with_qemus_agent_completes(),
        "crypto_instrument_prints_its_table" => crypto_instrument_prints_its_table(test_config),
        "the_compositor_drives_the_virtio_gpu" => the_compositor_drives_the_virtio_gpu(),
        "a_window_is_drawn_at_the_scale_and_relaid_at_the_next" => a_window_is_drawn_at_the_scale_and_relaid_at_the_next(),
        other => Err(format!("unknown machine test {other}")),
    }
}

/// `tests/guest/rust-tests`' `scale_window`'s fills, before and after the
/// relay.
const SCALE_BEFORE: [u8; 3] = [0x00, 0xff, 0x7f];
const SCALE_AFTER: [u8; 3] = [0xff, 0x00, 0x7f];
/// The compositor's focused title bar (`system/compositor/src/render.rs`).
const FOCUSED_TITLE: [u8; 3] = [0x3a, 0x3a, 0x4e];

/// Boot tests/images/scalecase.toml, whose compositor starts at scale 2, and
/// run `scale_window relay`: its 200x100 logical window is filled and on the
/// panel 400x200 under a title bar 56 rows tall; `screen scale 1` relays it,
/// and its new fill is on the panel 200x100 under one 28 rows tall. Enter,
/// pressed once each panel is read, is what lets the job go on.
fn a_window_is_drawn_at_the_scale_and_relaid_at_the_next() -> Result<(), String> {
    const JOB: &str = "scale_window";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let mut qemu = QemuInstance::boot_with_options(
        &compile::repo_root().join("tests/images/scalecase.toml"),
        &[],
        &[(JOB.to_string(), bin)],
        BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_marker(&mut qemu, &mut console, "compositor: drawing at scale 2, the image's", "the compositor at scale 2")?;
    await_marker(&mut qemu, &mut console, ps2server::ARMED_BOTH, "ps2server to arm the i8042")?;
    writeln!(qemu.stdin_mut(), "run test_rs_{JOB} relay").map_err(|e| e.to_string())?;
    qemu.flush_stdin();
    let enter = |qemu: &QemuInstance| {
        let mut input = qemu::QmpInput::open(qemu.qmp_socket());
        input.keys(&[("ret", true)]);
        input.keys(&[("ret", false)]);
    };

    let from = console.len();
    await_marker_new(&mut qemu, &mut console, "scale_window: logical 200x100 drawn 400x200 at scale 2", from, "the window at 2")?;
    let at_two = window_drawn(&qemu.screendump(), SCALE_BEFORE)?;
    enter(&qemu);
    let from = console.len();
    await_marker_new(&mut qemu, &mut console, "scale_window: logical 200x100 drawn 200x100 at scale 1", from, "the window relaid at 1")?;
    let at_one = window_drawn(&qemu.screendump(), SCALE_AFTER)?;
    enter(&qemu);
    await_marker_new(&mut qemu, &mut console, &format!("===TEST_END test_rs_{JOB} exit=0==="), from, "the job's end")?;

    if at_two != ((400, 200), 56) || at_one != ((200, 100), 28) {
        return Err(format!(
            "at 2 the window is {:?} under a {}-row title bar and at 1 {:?} under {}: 400x200 under 56 \
             and 200x100 under 28 were asked\n{console}",
            at_two.0, at_two.1, at_one.0, at_one.1
        ));
    }
    let said = serial::Serial::named("the scale boot", console);
    said.must_say("compositor: scale 1, kept 1")?;
    said.must_be_clean()?;
    eprintln!("  [scale] 400x200 under 56 rows at 2, relaid 200x100 under 28 at 1");
    Ok(())
}

/// The extent of the pixels of `fill` on `dump`, and how many rows of the
/// focused title bar's colour stand above its middle column: the window's
/// content and its title bar, which a border row separates from the top of
/// the frame. Every pixel of the extent is `fill` but those a cursor drawn in
/// software covers.
fn window_drawn(dump: &screen::Ppm, fill: [u8; 3]) -> Result<((usize, usize), usize), String> {
    let at = |x: usize, y: usize| dump.pixels[y * dump.width + x];
    let (mut x0, mut y0, mut x1, mut y1, mut n) = (usize::MAX, usize::MAX, 0, 0, 0usize);
    for y in 0..dump.height {
        for x in 0..dump.width {
            if at(x, y) == fill {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                n += 1;
            }
        }
    }
    if n == 0 {
        return Err(format!("no pixel of {fill:02x?} on a {}x{} panel", dump.width, dump.height));
    }
    let (w, h) = (x1 - x0, y1 - y0);
    // A software cursor 40 pixels square at the most, over the window.
    if n + 40 * 40 < w * h {
        return Err(format!("{fill:02x?}'s {w}x{h} extent at ({x0}, {y0}) holds only {n} of its pixels"));
    }
    let middle = x0 + w / 2;
    let title = (0..y0).rev().take_while(|&y| at(middle, y) == FOCUSED_TITLE).count();
    Ok(((w, h), title))
}

/// The virtio-serial driver hands the clipboard's port to the agent, the host
/// announces its capabilities through it, and QEMU's vdagent answers the
/// agent's opening `REQUEST` with no data, since `-display none` gives QEMU's
/// clipboard no peer: the driver and the agent said each, in this order.
fn the_opening_exchange_with_qemus_agent_completes() -> Result<(), String> {
    const HANDED: &str = "virtio-console: carrying the clipboard's channel on port 1";
    const OPEN: &str = "vdagent: the host announces capabilities 0x00020060";
    const ANSWERED: &str = "vdagent: the host's clipboard, 0 bytes";
    let mut qemu = QemuInstance::boot_with_options(
        &compile::repo_root().join("tests/images/clipboardcase.toml"),
        &[],
        &[],
        BootOptions { profile: qemu::Profile::HeadlessVirtioGpu, ..Default::default() },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_guest(&mut qemu, &mut console, "the agent's opening exchange", |t| t.contains(ANSWERED))
        .map_err(|e| format!("{e}\n{console}"))?;
    let said = serial::Serial::named("the clipboard boot", console);
    said.must_be_clean()?;
    let text = said.text();
    match (text.find(HANDED), text.find(OPEN), text.find(ANSWERED)) {
        (Some(handed), Some(open), Some(answered)) if handed < open && open < answered => Ok(()),
        _ => Err(format!("the boot did not say {HANDED:?}, {OPEN:?} and {ANSWERED:?} in that order:\n{text}")),
    }
}

/// The compositor drives QEMU's virtio-gpu through its `pci:03:80:00/1af4:1050`
/// claim: it starts at the size the device prefers, QEMU's default
/// 1280x800, and its first frame reaches the scanout; then a VNC client
/// resizes the host's display to 1024x768, the device raises its display
/// event, and the compositor flips to that mode and draws it. The image's
/// `scale=2` is held down to 1.75 and then 1.5, the steps those modes hold,
/// so the flip relays the desktop at a new scale.
///
/// "Drawn" is a screendump of the scanout at the mode's size holding the
/// wallpaper's many colours: a scanout no frame reached is one colour, or
/// QEMU's two-colour notice that the guest set none.
fn the_compositor_drives_the_virtio_gpu() -> Result<(), String> {
    const FIRST: &str = "compositor: display 1280x800";
    const FRAME: &str = "compositor: the first frame is on the panel";
    const FOLLOWED: &str = "compositor: mode 1024x768";
    let mut qemu = QemuInstance::boot_with_options(
        &compile::repo_root().join("tests/images/clipboardcase.toml"),
        &[],
        &[],
        BootOptions { profile: qemu::Profile::HeadlessVirtioGpu, qmp: true, vnc: true, ..Default::default() },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_guest(&mut qemu, &mut console, "the compositor's first frame", |t| t.contains(FRAME))
        .map_err(|e| format!("{e}\n{console}"))?;
    let drawn = |dump: &screen::Ppm, (width, height)| -> Result<(), String> {
        let colours: BTreeSet<[u8; 3]> = dump.pixels.iter().copied().collect();
        if (dump.width, dump.height) != (width, height) || colours.len() < 64 {
            return Err(format!(
                "the scanout is {}x{} in {} colour(s), where the desktop at {width}x{height} was drawn",
                dump.width,
                dump.height,
                colours.len()
            ));
        }
        Ok(())
    };
    drawn(&qemu.screendump(), (1280, 800)).map_err(|e| format!("{e}\n{console}"))?;

    let _client = common::vnc::resize(qemu.vnc_socket(), 1024, 768)?;
    await_guest(&mut qemu, &mut console, "the compositor following the host's display", |t| t.contains(FOLLOWED))
        .map_err(|e| format!("{e}\n{console}"))?;
    // The flip's frame is drawn into the new scanout before it is shown.
    drawn(&qemu.screendump(), (1024, 768)).map_err(|e| format!("{e}\n{console}"))?;

    let said = serial::Serial::named("the virtio-gpu boot", console);
    said.must_be_clean()?;
    said.must_say(FIRST)?;
    // The image's 2, held down to the step each mode holds, on the start and
    // on the flip's relay.
    said.must_say("compositor: drawing at scale 1.75, the image's")?;
    said.must_say("compositor: scale 2 leaves 1024x768 less than the least screen; drawing at 1.5")?;
    eprintln!("  [virtio-gpu] 1280x800 drawn at 1.75, then 1024x768 followed and drawn at 1.5");
    Ok(())
}

/// A machine with one NVMe disk and no USB controller boots the image off
/// that disk to ready, and says nothing a clean boot does not.
fn nvme_disk_boots(test_config: &Path) -> Result<(), String> {
    let options = BootOptions { profile: qemu::Profile::HeadlessNoUsb, ..Default::default() };
    if let Some(usb) = qemu::profile_argv(&options).iter().find(|arg| arg.contains("usb") || arg.contains("xhci")) {
        return Err(format!("this machine was to have no USB controller, and QEMU is given {usb}"));
    }
    let qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let log = serial::Serial::boot(&qemu);
    log.must_be_clean()?;
    // Every role is on this disk, so each is served off the nvme driver's
    // sessions through the block service, or the boot left it absent.
    for role in NVME_ROLES {
        log.must_say(&format!("fileserver: {role}"))?;
    }
    Ok(())
}

/// The NVMe driver's own account of the designation, which must be that it
/// wrote it: the line, or why there is none.
fn nvme_wrote_the_designation(text: &str) -> Result<&str, String> {
    let said: Vec<&str> = text.lines().filter(|l| l.contains("nvme ") && l.contains(": designation of ")).collect();
    match said[..] {
        [one] if one.ends_with(": Written") => Ok(one),
        _ => Err(format!("the NVMe driver did not say it wrote one designation: {said:?}")),
    }
}

/// The first boot designated the machine's DATA partition, judged and
/// written by the NVMe driver and read back, through the runner's
/// authorization; the second formatted it and serves it, and neither served
/// DATA from memory.
fn bench_data_designated(first: &serial::Serial, second: &serial::Serial) -> Result<(), String> {
    first.must_say("supervisor: test-runner is authorized for `designate:data`")?;
    nvme_wrote_the_designation(first.text())?;
    first.must_say("the DATA partition is designated: the stamp is on the disk and read back")?;
    first.must_not_say("are in memory and will not survive a reboot")?;
    second.must_say("fileserver: block 0 designates this partition for ToyOS; formatting it")?;
    let serving = second.must_say("fileserver: Data serving ")?;
    if !serving.contains(" — bcachefs") {
        return Err(format!("DATA is served by no bcachefs volume: {serving:?}"));
    }
    second.must_not_say("are in memory and will not survive a reboot")?;
    second.must_not_say("DATA is absent this boot")?;
    Ok(())
}

/// A boot of the designation image beside [`common::hostdisk`]'s disk: a
/// judge-only `disks designate data` writes nothing and says it would, the
/// committed one is written by the NVMe driver and read back, and once the
/// machine has stopped the disk's DATA partition begins with the stamp for its
/// size and every other byte of the disk is as it was.
fn the_data_partition_beside_the_stick_is_designated() -> Result<(), String> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images/designatecase.toml");
    let options = BootOptions { profile: qemu::Profile::MetalBesideAHostDisk, qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
    let mut console = qemu.boot_log().to_string();
    qemu::await_marker(&mut qemu, &mut console, "fileserver: Data serving", "DATA's file server to serve")?;
    let judged = qemu.run_test("disks designate data", Duration::from_secs(60));
    if judged.exit_code != Some(0) || !judged.stdout.contains("would be designated; nothing was written") {
        return Err(format!("the judge-only designation: {:?}\n{}", judged.exit_code, judged.serial));
    }
    let done = qemu.run_test("disks designate data --commit", Duration::from_secs(60));
    if done.exit_code != Some(0) || !done.stdout.contains("the stamp is on the disk and read back") {
        return Err(format!("the designation: {:?}\n{}", done.exit_code, done.serial));
    }
    eprintln!("  [designate] {}", nvme_wrote_the_designation(&done.serial)?);
    qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET)).power_button();
    let by = qemu.budget(Duration::from_secs(60));
    console.push_str(&qemu.await_exit(by)?);
    let log = serial::Serial::named("the boot", console);
    log.must_say("supervisor: test-runner is authorized for `designate:data`")?;
    log.must_say("fileserver: the DATA partition holds no volume of ours and no designation stamp; DATA is absent this boot")?;

    let after = std::fs::read(qemu.nvme_image()).map_err(|e| e.to_string())?;
    let mut want = common::hostdisk::bytes();
    let (at, len) = common::hostdisk::data_span();
    let at = at as usize;
    want[at..at + 4096].copy_from_slice(&bcachefs::designation(len / 4096));
    match want.iter().zip(&after).position(|(w, a)| w != a) {
        None if want.len() == after.len() => Ok(()),
        None => Err(format!("the disk was {} bytes and is {}", want.len(), after.len())),
        Some(byte) => Err(format!("byte {byte} (sector {}) is not what the designation alone leaves", byte / 512)),
    }
}

/// `hostdisk::left_alone` of a stick boot beside [`common::hostdisk`]'s disk,
/// whose four partitions the driver of QEMU's controller under Intel's id
/// named, and every byte of the disk as it was once the machine has stopped.
fn a_host_disk_beside_the_stick_is_left_alone(test_config: &Path) -> Result<(), String> {
    let options = BootOptions { profile: qemu::Profile::MetalBesideAHostDisk, qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let mut console = qemu.boot_log().to_string();
    qemu::await_marker(&mut qemu, &mut console, "fileserver: Data serving", "DATA's file server to serve")?;
    qemu::await_marker(&mut qemu, &mut console, toyos_logstream::LOG_OPENED, "logkeeper to open the log")?;
    qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET)).power_button();
    let by = qemu.budget(Duration::from_secs(60));
    console.push_str(&qemu.await_exit(by)?);
    let log = serial::Serial::named("the boot", console);
    log.must_say("vendor=8086 device=5845 prog_if=02")?;
    // This disk's DATA is never designated: what it is judged on is that
    // nothing writes to a disk that is not ours.
    match common::hostdisk::left_alone(&log, &[])? {
        (4, common::hostdisk::Data::Foreign) => {}
        (named, data) => return Err(format!("the driver named {named} partitions of a disk that has 4, and DATA was {data:?}")),
    }
    let after = std::fs::read(qemu.nvme_image()).map_err(|e| e.to_string())?;
    let before = common::hostdisk::bytes();
    match before.iter().zip(&after).position(|(b, a)| b != a) {
        None if before.len() == after.len() => Ok(()),
        None => Err(format!("the disk was {} bytes and is {}", before.len(), after.len())),
        Some(at) => Err(format!("the disk was written: byte {at} (sector {}) differs", at / 512)),
    }
}

/// How each file server says its role is served off `nvme_disk_boots`'s disk.
const NVME_ROLES: [&str; 3] = [
    "Log serving /log — FAT32, ",
    "Boot serving /boot — FAT32 read-only, ",
    "Data serving /apps, /config, /home, /state — bcachefs, ",
];
/// Run `command` as a job of the boot, to exit 0: everything said in its
/// window, the kernel's records with the program lines.
fn job_window(qemu: &mut QemuInstance, command: &str) -> Result<String, String> {
    let result = qemu.run_test(command, Duration::from_secs(60));
    if let Some(why) = &result.error {
        return Err(format!("`{command}`: {why}\nit said:\n{}", result.serial));
    }
    if result.exit_code != Some(0) {
        return Err(format!("`{command}` ended {:?}:\n{}", result.exit_code, result.serial));
    }
    Ok(result.serial)
}

/// The xHCI controller a job's claim took, by address and card: handed over,
/// and not the one the supervisor says `@boot` is, which the image's usbd row
/// drives where it is an xHCI.
fn spare_xhci(kernel: &serial::Serial, log: &serial::Serial) -> Result<(String, String), String> {
    let text = kernel.text();
    let boot = boot_controller(log)?;
    let mut spares: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let Some((at, rest)) = line.split_once("pcidev: PCI ").and_then(|(_, l)| l.split_once(" [")) else { continue };
        let Some((id, rest)) = rest.split_once(']') else { continue };
        let spare = (at.to_string(), id.to_string());
        if USB_XHCI.contains(&id) && rest.starts_with(" handed over") && at != boot && !spares.contains(&spare) {
            spares.push(spare);
        }
    }
    match <[_; 1]>::try_from(spares) {
        Ok([spare]) => Ok(spare),
        Err(spares) => Err(format!("not one xHCI controller beside the boot one at {boot} was handed over: {spares:?}")),
    }
}

/// The controller the supervisor says `@boot` is, as `bb:dd.f`.
fn boot_controller(log: &serial::Serial) -> Result<&str, String> {
    log.text()
        .lines()
        .find_map(|l| l.split_once(toyos_manifest::device::BOOT_SAID)?.1.split_whitespace().next())
        .ok_or_else(|| format!("the supervisor never said which controller `@boot` is:\n{}", log.text()))
}

/// The job that kills a holder of the spare and claims it again at once, in
/// `usbd_spare`'s boots.
const END_RETURNS_CLAIMS: &str = "end_returns_claims";

/// `end_returns_claims`'s summary: every round's claim granted after its
/// holder's end. The times it prints are evidence, never judged.
fn ends_returned_claims(said: &str) -> Result<String, String> {
    said.lines()
        .find_map(|l| l.split_once("end_returns_claims: ").map(|(_, rest)| rest))
        .filter(|rest| rest.contains("every claim granted after its holder's end"))
        .map(str::to_string)
        .ok_or_else(|| format!("end_returns_claims never said every claim came back:\n{said}"))
}

/// What usbd on the controller at `at` says once the bring-up's No-Op has
/// completed: its ring and the interrupt through the claim both work, since
/// usbd reads its event ring only when an interrupt record says to.
fn usbd_noop(at: &str) -> String {
    format!("usbd {at}: a No-Op command completed")
}

/// The spare no `drives` row names, handed over to `usbd_spare`'s claim and
/// driven by usbd as far as its No-Op, answered by an interrupt through the
/// claim, with every event announced by one. Then the two controls: the
/// function aimed past every grant is the kernel's `DMA FAULT` record against
/// its slot and the claim's refusal, and a claim on the boot controller, which
/// the image's usbd holds, is refused as held. Between them, a holder of the
/// spare is killed and the claim asked for again at once, each round, which
/// only a release finished before the end grants.
fn usbd_drives_the_spare(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "usbd_spare";
    const SPARE: &str = "1b36:000d";
    let at = qemu::SPARE_AT;
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, JOB);
    let ends = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, END_RETURNS_CLAIMS);
    let options = BootOptions { profile: qemu::Profile::HeadlessUsbSpare, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[(JOB.to_string(), bin), (END_RETURNS_CLAIMS.to_string(), ends)],
        options,
    );
    serial::Serial::boot(&qemu).must_be_clean()?;

    let said = job_window(&mut qemu, "test_rs_usbd_spare")?;
    let run = serial::Serial::named("usbd_spare", said.clone());
    run.must_be_clean()?;
    run.must_say(&usbd_noop(at))?;
    run.must_say("usbd_spare: settled: usb.events.unannounced = 0")?;
    let handed = format!("pcidev: PCI {at} [{SPARE}] handed over, vector ");
    if said.matches(&handed).count() != 1 {
        return Err(format!("{at} was not handed over once, to the one usbd:\n{said}"));
    }
    run.must_say(&format!("PCI {at}: msix address="))?;

    let faulted = job_window(&mut qemu, "test_rs_usbd_spare fault")?;
    let fault = serial::Serial::named("usbd_spare fault", faulted.clone());
    fault.must_say("usbd_spare: the claim refuses its interrupt read: Io")?;
    let aimed = faulted
        .lines()
        .find_map(|l| l.split_once("usbd_spare: the command ring aimed at ")?.1.split_once(',').map(|(a, _)| a.to_string()))
        .ok_or_else(|| format!("the job never said where it aimed:\n{faulted}"))?;
    let record = faulted
        .lines()
        .find(|l| l.contains("iommu: DMA FAULT owner=process") && l.contains(&format!("stream={at} ")))
        .ok_or_else(|| format!("no DMA FAULT record names {at}:\n{faulted}"))?;
    let addr = record.split_once(" addr=").and_then(|(_, a)| a.split_whitespace().next()).unwrap_or_default();
    let hex = |text: &str| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok();
    if hex(addr) != hex(&aimed) {
        return Err(format!("the record faults {addr}, and the job aimed at {aimed}: {record}"));
    }
    eprintln!("  [usbd] {at}: handed over, its No-Op answered; aimed at {aimed}: {}", record.trim());
    let ended = job_window(&mut qemu, "test_rs_end_returns_claims")?;
    let line = ends_returned_claims(&ended)?;
    eprintln!("  [usbd] {line}");

    // The control: the boot controller, which the image's usbd holds.
    let said = job_window(&mut qemu, "test_rs_usbd_spare refused")?;
    serial::Serial::named("usbd_spare refused", said).must_say("usbd_spare: the claim on pci:1033:0194 was refused: AlreadyExists")?;
    Ok(())
}

/// A machine behind QEMU's AMD unit, which `said` describes, refuses the
/// spare's claim for `why`, before a vector is armed.
fn usbd_refused_on_amdvi(test_config: &Path, profile: qemu::Profile, said: &str, why: &str) -> Result<(), String> {
    const JOB: &str = "usbd_spare";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let options = BootOptions { profile, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], options);
    serial::Serial::boot(&qemu).must_say(said)?;
    let run = job_window(&mut qemu, "test_rs_usbd_spare unhanded")?;
    claims::refused_at(&serial::Serial::named("usbd_spare", run), qemu::SPARE_AT, why)
}

/// A `drives` row's processes on a q35 with two `edu`, which the kernel drives
/// neither of: the kernel hands each function over once, two `pciholder`s
/// each say they hold one of the two, and the job reads both still held,
/// each by a process of its own.
fn a_drives_row_holds_one_process_per_function() -> Result<(), String> {
    const JOB: &str = "drives_judged";
    const HOLDING: &str = "pciholder: holding PCI ";
    const HANDED: &str = " [1234:11e8] handed over, ";
    let options = BootOptions { profile: qemu::Profile::HeadlessTwoEdu, ..Default::default() };
    let edus = qemu::profile_argv(&options).iter().filter(|arg| *arg == "edu").count();
    if edus != 2 {
        return Err(format!("the machine was to have two edu functions, and QEMU is given {edus}"));
    }
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let config = compile::repo_root().join("tests/images/drivescase.toml");
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[(JOB.to_string(), bin)], options);
    let mut console = format!("{}\n", qemu.boot_log());
    await_guest(&mut qemu, &mut console, "both holders' lines", |t| t.matches(HOLDING).count() >= 2)
        .map_err(|e| format!("{e}\n{console}"))?;
    let said = serial::Serial::named("the drives boot", console.clone());
    said.must_be_clean()?;
    let mut handed: Vec<&str> =
        console.lines().filter_map(|l| l.split_once("pcidev: PCI ")?.1.split_once(HANDED).map(|(at, _)| at)).collect();
    let mut holding: Vec<&str> =
        console.lines().filter_map(|l| l.split_once(HOLDING)?.1.split_whitespace().next()).collect();
    handed.sort_unstable();
    holding.sort_unstable();
    // Each function handed over once, to one holder: a list, so a second
    // hand-over or a second holder of one function is red.
    if handed.len() != 2 || handed[0] == handed[1] || holding != handed {
        return Err(format!("the kernel handed over {handed:?} and the holders say they hold {holding:?}:\n{console}"));
    }
    let judged = job_window(&mut qemu, "test_rs_drives_judged")?;
    let line = judged
        .lines()
        .find(|l| l.contains("drives_judged: PCI "))
        .ok_or_else(|| format!("the job never said who holds the functions:\n{judged}"))?;
    eprintln!("  [drives] {}", line.trim());
    Ok(())
}

/// The AMD bench's spare xHCI ([`spare_xhci`]) left to a claim and driven by usbd: handed over
/// with its MSI-X table's BAR, armed through a present entry of its own table
/// at its claim's vector, its No-Op answered by that interrupt; then aimed
/// past its grant and recorded against the claim at the aimed address; then
/// its holder's rewrite of its own table entry to a vector not its own
/// refused and recorded against the claim, which usbd ends on, never told of
/// its No-Op.
fn amdvi_usbd(kernel: &serial::Serial, log: &serial::Serial, after: &serial::Serial) -> Result<(), String> {
    let (at, id) = spare_xhci(kernel, log)?;
    let at = at.as_str();
    let handed = format!("pcidev: PCI {at} [{id}] handed over, vector ");
    let vector = kernel
        .text()
        .lines()
        .find_map(|l| l.split_once(&handed)?.1.split_whitespace().next().map(str::to_string))
        .ok_or_else(|| format!("{at} was never handed over:\n{}", kernel.text()))?;
    kernel.must_say(&format!("PCI {at}: msix address="))?;
    kernel.must_say(&format!("iommu: amdvi irte source={at} vector={vector} p=1 "))?;
    log.must_say(&usbd_noop(at))?;

    let aimed = log
        .text()
        .lines()
        .find_map(|l| l.split_once("usbd_spare: the command ring aimed at ")?.1.split_once(',').map(|(a, _)| a.to_string()))
        .ok_or_else(|| format!("the fault arm never said where it aimed:\n{}", log.text()))?;
    let hex = |text: &str| u64::from_str_radix(text.trim_start_matches("0x"), 16).ok();
    // Every record of the spare's: the boot's other rows leave these to this
    // one ([`no_fault_but_the_spares`]), so each must be one the jobs staged.
    let records: Vec<&str> =
        kernel.text().lines().filter(|l| l.contains("iommu: DMA FAULT") && l.contains(&format!(" stream={at} "))).collect();
    let process = |l: &&&str| l.contains("iommu: DMA FAULT owner=process ");
    let aimed_at = records
        .iter()
        .find(|l| process(l) && l.contains(" access=read ") && l.split_once(" addr=").and_then(|(_, a)| hex(a.split_whitespace().next()?)) == hex(&aimed))
        .ok_or_else(|| format!("no record of {at} names the aimed {aimed}: {records:?}"))?;
    log.must_say("usbd_spare: the claim refuses its interrupt read: Io")?;

    let rewritten = log.must_say("usbd_spare: MSI-X entry 0's data ")?;
    log.must_say(&format!("usbd {at}: ENDED — the claim refused its interrupt record (Io)"))?;
    log.must_say("usbd_spare: usbd ended on the claim's refusal")?;
    // Refused by the unit, not delivered to a vector nobody holds: the stop's
    // census names every vector the unclaimed gate took.
    let foreign_vector = rewritten
        .split_once(" rewritten to ")
        .and_then(|(_, rest)| rest.split(',').next())
        .ok_or_else(|| format!("{rewritten:?} names no vector"))?;
    // Logged inside the stop, past where the log file ends.
    let census = after.must_say("irq: unclaimed vectors")?;
    if census.split_whitespace().any(|word| word == foreign_vector) {
        return Err(format!("{foreign_vector} reached a CPU: {census}"));
    }
    // An interrupt's record states no read or write (48882 Table 57, `I`).
    let foreign = records
        .iter()
        .find(|l| process(l) && l.contains(" access=none ") && l.ends_with("IO_PAGE_FAULT"))
        .ok_or_else(|| format!("no record of {at}'s message on a vector not its own: {records:?}"))?;
    if records.len() != 2 {
        return Err(format!("{at} has {} fault records, not the aimed read and the foreign message alone: {records:?}", records.len()));
    }
    eprintln!("  [amdvi] {at}: handed over on {vector}, its No-Op answered through its own entry");
    eprintln!("  [amdvi] aimed at {aimed}: {}", aimed_at.trim());
    eprintln!("  [amdvi] a vector not its own: {}", foreign.trim());
    Ok(())
}

/// Two HDA controllers, the first with no codec and the second with an
/// output one ([`qemu::Profile::MetalTwoHda`]), each with an `hda` driver of
/// its own: the first plays nothing and says why, the second offers its
/// output, and soundserver plays to it. Then the benches' master job on that
/// output: reset, CORB/RIRB, the walk, the path and the master's readback,
/// each behind VT-d and through the claim.
///
/// Not the tone jobs: cpal holds a stream's close to a fade and a ring of
/// periods at the device's rate, and QEMU's controller completes periods in
/// bursts of up to a ring paced by the host, so that bound is a time only the
/// benches judge.
fn hda_plays_to_the_controller_that_reaches_an_output(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "volume_step";
    const OFFERED: &str = ": path configured in ";
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bins = [(JOB.to_string(), qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, JOB))];
    let options = BootOptions { profile: qemu::Profile::MetalTwoHda, ..Default::default() };
    let controllers = qemu::profile_argv(&options).iter().filter(|arg| arg.starts_with("ich9-intel-hda")).count();
    if controllers != 2 {
        return Err(format!("the machine was to have two Intel HDA controllers, and QEMU is given {controllers}"));
    }
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &bins, options);
    let mut console = format!("{}\n", qemu.boot_log());
    // Both drivers' verdicts and soundserver's: each comes when its walk does,
    // which can be after the ready marker.
    let decided = |t: &str| t.contains(audio::PLAYING) && t.contains("plays nothing: ");
    await_guest(&mut qemu, &mut console, "both drivers' verdicts and soundserver's", decided)
        .map_err(|e| format!("{e}\n{console}"))?;
    console += &job_window(&mut qemu, &format!("test_rs_{JOB}"))?;
    let said = serial::Serial::named("the two-controller boot", console.clone());
    said.must_be_clean()?;
    said.must_not_say("iommu: DMA FAULT")?;
    let driver = |line: &str, what: &str| line.split_once("] hda ")?.1.split_once(what).map(|(at, _)| at.to_string());
    let silent: Vec<String> = console.lines().filter_map(|l| driver(l, ": plays nothing: ")).collect();
    let offered: Vec<String> = console.lines().filter_map(|l| driver(l, OFFERED)).collect();
    let [silent] = silent.as_slice() else { return Err(format!("not one driver played nothing: {silent:?}\n{console}")) };
    let [offered] = offered.as_slice() else { return Err(format!("not one driver offered: {offered:?}\n{console}")) };
    said.must_say(&format!("hda {silent}: plays nothing: the controller refused: no codec is on its link"))?;
    if silent > offered {
        return Err(format!("the codec-less controller {silent} was to enumerate before {offered}:\n{console}"));
    }
    if console.matches(audio::PLAYING).count() != 1 {
        return Err(format!("soundserver did not play to one output:\n{console}"));
    }
    Ok(())
}

/// Every xHCI card a boot of the test images can have: QEMU's two, the T14's
/// two and the AMD bench's.
const USB_XHCI: [&str; 5] = ["1033:0194", "8086:a0ed", "8086:9a13", "1022:1639", "1b36:000d"];

/// Every xHCI controller handed over to a usbd: by the kernel's hand-over
/// record, of [`USB_XHCI`].
fn usb_controllers(kernel: &serial::Serial) -> Vec<String> {
    kernel
        .text()
        .lines()
        .filter_map(|l| {
            let (at, rest) = l.split_once("pcidev: PCI ")?.1.split_once(" [")?;
            let (id, rest) = rest.split_once(']')?;
            (USB_XHCI.contains(&id) && rest.starts_with(" handed over")).then(|| at.to_string())
        })
        .collect()
}

/// No DMA refused on the AMD bench's testcases boot but the spare's: that boot
/// also carries `amdvi_usbd_drives_its_own_entry`, whose jobs aim the spare
/// ([`spare_xhci`]) past its grant and at a vector not its own, and which
/// judges those records itself.
fn no_fault_but_the_spares(kernel: &serial::Serial, log: &serial::Serial) -> Result<(), String> {
    kernel.alive()?;
    let spares = spare_xhci(kernel, log).map(|(at, _)| format!(" stream={at} ")).ok();
    let theirs = |l: &str| spares.as_deref().is_some_and(|spares| l.contains(spares));
    match kernel.text().lines().find(|l| l.contains("iommu: DMA FAULT") && !theirs(l)) {
        Some(line) => Err(format!("a DMA refusal no job asked for: {line:?}")),
        None => Ok(()),
    }
}

/// The nvme driver's word that a session opened on the disk it drives at
/// `at`: block I/O through that controller.
fn nvme_session(at: &str) -> String {
    format!("nvme {at}: session ")
}

/// Every AMD-Vi unit translating, the boot disk's controller and every xHCI
/// controller usbd drives on a domain of its own, the boot disk read and
/// written through its one, and no DMA refused.
fn amdvi_translates(kernel: &serial::Serial, log: &serial::Serial) -> Result<(), String> {
    let text = kernel.text();
    let units: Vec<&str> =
        text.lines().filter(|l| l.contains("iommu: amdvi unit") && l.contains(" translating control=")).collect();
    if units.is_empty() {
        return Err(format!("no AMD-Vi unit is translating:\n{text}"));
    }
    kernel.must_not_say("isolates no device")?;
    no_fault_but_the_spares(kernel, log)?;
    let boot = boot_controller(log)?;
    let mut controllers = usb_controllers(kernel);
    controllers.push(boot.to_string());
    for at in &controllers {
        kernel.must_say(&format!("iommu: {at} moves to domain"))?;
    }
    log.must_say(&nvme_session(boot))?;
    for unit in units {
        eprintln!("  [amdvi] {}", unit.trim());
    }
    eprintln!("  [amdvi] {} controller(s) on domains of their own, the boot disk served", controllers.len());
    Ok(())
}

/// Every AMD-Vi unit remapping (`GAEn`, MMIO 0018h bit 17) and, once the
/// kernel's drivers are settled, filtering SMIs and logging each it refuses
/// (`SmiFEn` bit 22, `SmiFLogEn` bit 24); every I/O APIC the MADT has named
/// by the IVRS; and the boot disk's controller and each xHCI controller usbd
/// drives armed through a present entry of its own table at the vector its
/// message carries, the boot disk served by those interrupts.
fn amdvi_remaps(kernel: &serial::Serial, log: &serial::Serial) -> Result<(), String> {
    let text = kernel.text();
    let remaps = kernel.must_say("iommu: amdvi remaps every message through its requester's table")?;
    let control = |line: &str| {
        line.split_once(" control=")
            .and_then(|(_, rest)| u64::from_str_radix(rest.split([' ', ',']).next()?.trim_start_matches("0x"), 16).ok())
            .ok_or_else(|| format!("{line:?} names no control"))
    };
    let units: Vec<&str> =
        text.lines().filter(|l| l.contains("iommu: amdvi unit") && l.contains(" translating control=")).collect();
    if units.is_empty() {
        return Err(format!("no AMD-Vi unit is translating:\n{text}"));
    }
    for unit in &units {
        if control(unit)? & 1 << 17 == 0 {
            return Err(format!("{unit:?} leaves GAEn clear"));
        }
    }
    let filters: Vec<&str> = text.lines().filter(|l| l.contains(" SMI filter on control=")).collect();
    if filters.len() != units.len() {
        return Err(format!("{} unit(s) translate and {} filter SMIs: {filters:?}", units.len(), filters.len()));
    }
    for filter in &filters {
        if control(filter)? & (1 << 22 | 1 << 24) != 1 << 22 | 1 << 24 {
            return Err(format!("{filter:?} leaves SmiFEn or SmiFLogEn clear"));
        }
    }
    let apics = text.lines().filter(|l| l.contains("ioapic: id=") && l.contains(" at 0x")).count();
    if !remaps.contains(&format!(" {apics} I/O APIC(s) named")) {
        return Err(format!("{remaps:?} does not name the {apics} I/O APIC(s) the MADT has"));
    }
    no_fault_but_the_spares(kernel, log)?;
    let boot = boot_controller(log)?;
    let mut controllers = usb_controllers(kernel);
    controllers.push(boot.to_string());
    for at in &controllers {
        let data = text
            .lines()
            .find_map(|l| l.split_once(&format!("PCI {at}: msix address="))?.1.split_once(" data=").map(|(_, d)| d.trim()))
            .ok_or_else(|| format!("{at} was not armed on MSI-X:\n{text}"))?;
        let vector = u32::from_str_radix(data.trim_start_matches("0x"), 16).map_err(|e| format!("{data}: {e}"))? & 0xFF;
        kernel.must_say(&format!("iommu: amdvi irte source={at} vector={vector:#04x} p=1 "))?;
    }
    log.must_say(&nvme_session(boot))?;
    eprintln!("  [amdvi] {}", remaps.trim());
    for filter in filters {
        eprintln!("  [amdvi] {}", filter.trim());
    }
    for refused in text.lines().filter(|l| l.contains("iommu: amdvi unit") && l.contains(" said so far")) {
        eprintln!("  [amdvi] {}", refused.trim());
    }
    eprintln!("  [amdvi] {} controller(s) armed through their own entries, the boot disk served", controllers.len());
    Ok(())
}

/// The T14's spare controller, the first free one, left to a claim and driven
/// by usbd: the claim was
/// handed over with its MSI in remappable format and its remapping entry
/// written, the message reached its claim, and the No-Op completed. What
/// reset the function advertises is read off the release and printed, not
/// judged.
fn usbd_on_metal(kernel: &serial::Serial, log: &serial::Serial) -> Result<(), String> {
    let (at, id) = spare_xhci(kernel, log)?;
    let message = kernel
        .text()
        .lines()
        .find_map(|l| l.split_once(&format!("PCI {at}: msi address="))?.1.split_whitespace().next().map(str::to_string))
        .ok_or_else(|| format!("{at} was not armed on MSI:\n{}", kernel.text()))?;
    let address = u32::from_str_radix(message.trim_start_matches("0x"), 16).map_err(|e| format!("{message}: {e}"))?;
    // Interrupt Format, bit 4 of the address: the message names a remapping
    // entry and no vector of its own (VT-d 3.4 §5.1.5.2).
    if address & (1 << 4) == 0 {
        return Err(format!("{at}'s MSI address {address:#010x} is compatibility format"));
    }
    kernel.must_say(&format!(" source={at} p=1 "))?;
    kernel.must_say("took its first message on vector")?;
    log.must_say(&usbd_noop(&at))?;
    for released in kernel.text().lines().filter(|l| l.contains(&format!("pcidev: PCI {at} [{id}] released; "))) {
        eprintln!("  [usbd] {}", released.trim());
    }
    eprintln!("  [usbd] {at}: handed over, MSI {address:#010x} through its remapping entry, the No-Op answered");
    Ok(())
}

/// [`MACHINE_TESTS`]' `vector_state_survives_a_switch`: the shared boot's
/// `vector_state` alone on two CPUs, so five threads time-share them.
fn vector_state_survives_a_switch(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "vector_state";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let mut qemu =
        QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_vector_state", Duration::from_secs(300));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    let Some(line) = result.stdout.lines().find(|l| l.contains("vector_state: ")) else {
        return Err(format!("the job never said what it held:\n{}", result.stdout));
    };
    eprintln!("  [claims] {}", line.trim());
    Ok(())
}

/// `std_threads` on a guest of [`BootOptions`]' CPUs: it asserts what it
/// frees itself, and the CPU count it prints is held here to the CPUs the
/// guest was booted with.
fn std_threads_count_the_cpus_and_give_back_ended_threads(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "std_threads";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let options = BootOptions::default();
    let booted = options.smp;
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], options);
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_std_threads", Duration::from_secs(300));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    let counted = format!("std_threads: {booted} CPUs");
    if !result.stdout.lines().any(|l| l.trim_end().ends_with(&counted)) {
        return Err(format!("the guest was booted with {booted} CPUs and std counted otherwise:\n{}", result.stdout));
    }
    for line in result.stdout.lines().filter(|l| l.contains("std_threads: ")) {
        eprintln!("  [std] {}", line.trim());
    }
    Ok(())
}

/// `kernel_objects_refused` alone in its boot, which ends clean.
fn kernel_objects_refused_alone(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "kernel_objects_refused";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_kernel_objects_refused", Duration::from_secs(300));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    for line in result.stdout.lines().filter(|l| l.contains("PASS: ")) {
        eprintln!("  [refused] {}", line.trim());
    }
    Ok(())
}

/// `memory_floor --input` alone in `tests/images/floorcase.toml`'s boot, a key
/// typed once every frame is held: the job holds what the controls did, and the
/// supervisor said it was refused the starved service's restart, then started
/// it once memory came back.
fn memory_floor_holds_the_controls() -> Result<(), String> {
    const JOB: &str = "memory_floor";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let mut qemu = QemuInstance::boot_with_options(
        &compile::repo_root().join("tests/images/floorcase.toml"),
        &[],
        &[(JOB.to_string(), bin)],
        BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() },
    );
    let mut console = format!("{}\n", qemu.boot_log());
    await_marker(&mut qemu, &mut console, ps2server::ARMED_BOTH, "ps2server to arm the i8042")?;
    let from = console.len();
    writeln!(qemu.stdin_mut(), "run test_rs_{JOB} --input").map_err(|e| e.to_string())?;
    qemu.flush_stdin();
    let said = |console: &str, why: String| format!("{why}\nthe boot said:\n{}", &console[from..]);
    await_marker_new(&mut qemu, &mut console, MEMORY_FLOOR_PRESS, from, "the greedy job to hold every frame").map_err(|why| said(&console, why))?;
    {
        let mut input = qemu::QmpInput::open(qemu.qmp_socket());
        input.keys(&[("a", true)]);
        input.keys(&[("a", false)]);
    }
    await_marker_new(&mut qemu, &mut console, &format!("===TEST_END test_rs_{JOB} exit=0==="), from, "the job's end").map_err(|why| said(&console, why))?;
    let said = &console[from..];
    for line in [
        "could not start again for want of memory",
        "supervisor: starveling started again as pid ",
    ] {
        if !said.contains(line) {
            return Err(format!("the supervisor never said {line:?}\n{said}"));
        }
    }
    // Whatever else the exhaustion ended, a file server among them, starts
    // again once the memory is given back.
    for refused in said.lines().filter(|l| l.contains("could not start again for want of memory")) {
        let Some(service) = refused.split("supervisor: ").nth(1).and_then(|rest| rest.split(" (pid").next()) else { continue };
        if !said.contains(&format!("supervisor: {service} started again as pid ")) {
            return Err(format!("{service} was refused its restart for memory and never started again\n{said}"));
        }
        eprintln!("  [floor] {service} was refused its restart for memory and started again");
    }
    for line in said.lines().filter(|l| l.contains("PASS: ") || l.contains("memory_floor: ") || l.contains("supervisor: starveling")) {
        eprintln!("  [floor] {}", line.trim());
    }
    serial::Serial::named("the memory_floor boot", console).must_be_clean()
}

/// What `memory_floor` says once its greedy job holds every frame, the host's
/// cue to type (`tests/guest/rust-tests/src/bin/memory_floor.rs`).
const MEMORY_FLOOR_PRESS: &str = "memory_floor: every frame is held; press a key";

/// Ring's known answers, then both builds of the parity instrument, in one
/// boot: each ends cleanly and their lines read back as one whole table.
fn crypto_instrument_prints_its_table(test_config: &Path) -> Result<(), String> {
    let dir = compile::repo_root().join("tests/guest/rust-tests");
    let bins = vec![
        ("ring_kat".to_string(), qemu::build_toyos_bin(qemu::SUITE_ARCH, &dir, "ring_kat")),
        ("crypto_parity".to_string(), qemu::build_toyos_bin(qemu::SUITE_ARCH, &dir, "crypto_parity")),
        crypto_parity_unasserted(),
    ];
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &bins, BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let mut said = String::new();
    for job in CRYPTO_JOBS {
        let result = qemu.run_test(job, Duration::from_secs(CRYPTO_JOB_CEILING_SECS));
        if let Some(why) = &result.error {
            return Err(format!("{job}: {why}\nthe job said:\n{}", result.stdout));
        }
        if result.exit_code != Some(0) {
            return Err(format!("{job} ended {:?}:\n{}", result.exit_code, result.stdout));
        }
        said.push_str(&result.stdout);
    }
    let table = toyos_parity::read(said.lines())?;
    eprintln!("  [crypto] {} rows in each build; their timings are not judged under QEMU", table.asserted.len());
    Ok(())
}

/// A hang ceiling for one crypto job under QEMU, whose clock decides nothing
/// else.
const CRYPTO_JOB_CEILING_SECS: u64 = 300;

/// The guest port `test_rs_socket_export` listens on, which the boot forwards
/// a port of the host's loopback to. QEMU's user network listens on that host
/// port with a queue of one, and a macOS host resets a connect that finds it
/// full, so the peer connects only once the guest has answered its last
/// connection.
const SOCKET_EXPORT_PORT: u16 = 7070;

/// `test_rs_stream_export` and `test_rs_socket_export` on one boot of
/// `tests/images/netcase.toml`, the harness every socket's peer: a stream's
/// child's line, then its parent's, then the FIN once both holders are gone;
/// then a listener's and a datagram socket's child's datagram, its accept and
/// its line, and the parent's after it. Once over virtio, and once over QEMU's
/// `e1000e`, which the image's rows name no machine for: the I219 driver's
/// row matches it, and its link is the one netstack opens.
fn netstack_holders_exported_to_children() -> Result<(), String> {
    for (profile, driver) in [(qemu::Profile::Headless, "virtio-net"), (qemu::Profile::HeadlessE1000e, "82574")] {
        holders_exported_over(profile, driver)?;
        eprintln!("  [link] {driver}: a stream, a listener and a datagram socket exported to children");
    }
    Ok(())
}

fn holders_exported_over(profile: qemu::Profile, driver: &str) -> Result<(), String> {
    /// netstack's line once its lease landed: before it, a connect is refused.
    const LEASED: &str = "netstack: ready, at most";
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bins: Vec<(String, Vec<u8>)> =
        ["stream_export", "socket_export"].iter().map(|job| (job.to_string(), qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, job))).collect();
    // The host's end of the forward: a port found free and given up, which
    // QEMU binds as it starts and refuses to start without. Another process
    // can take it between the two, which fails the boot by name.
    let forwarded = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|probe| probe.local_addr())
        .map_err(|e| format!("no loopback port to forward: {e}"))?
        .port();
    let config = compile::repo_root().join("tests/images/netcase.toml");
    let options = BootOptions { profile, forward: Some((forwarded, SOCKET_EXPORT_PORT)), ..BootOptions::default() };
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
    serial::Serial::boot(&qemu).must_be_clean()?;
    let mut said = qemu.boot_log().to_string();
    if !said.contains(LEASED) {
        said.push_str(&qemu.drain_until(Duration::from_secs(60), |l| l.contains(LEASED)));
    }
    if !said.contains(LEASED) {
        return Err(format!("{driver}: netstack never said its lease landed\n{said}"));
    }
    let opened = format!("netstack: {driver}'s link is open");
    if !said.contains(&opened) {
        return Err(format!("{driver}: netstack leased without saying {opened:?}\n{said}"));
    }
    stream_exported_to_a_child(&mut qemu)?;
    sockets_exported_to_a_child(&mut qemu, forwarded)
}

/// `test_rs_stream_export`: the child's line, then the parent's, then the FIN
/// once both holders are gone.
fn stream_exported_to_a_child(qemu: &mut QemuInstance) -> Result<(), String> {
    use std::io::Read;
    let peer = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("no loopback listener: {e}"))?;
    let port = peer.local_addr().map_err(|e| format!("the listener's port: {e}"))?.port();
    // The peer's side on a thread of its own: the job's lines arrive while it runs.
    let heard = std::thread::spawn(move || -> Result<Vec<u8>, String> {
        let (mut conn, _) = peer.accept().map_err(|e| format!("the guest's connect: {e}"))?;
        conn.set_read_timeout(Some(Duration::from_secs(120))).map_err(|e| e.to_string())?;
        let mut all = Vec::new();
        conn.read_to_end(&mut all).map_err(|e| format!("the stream's bytes, until its FIN: {e} after {all:?}"))?;
        Ok(all)
    });
    let result = qemu.run_test(&format!("test_rs_stream_export {port}"), Duration::from_secs(120));
    if let Some(why) = &result.error {
        return Err(format!("{why}\n{}\nthe job said:\n{}", peer_verdict(heard), result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    let heard = heard.join().map_err(|_| "the peer's thread panicked".to_string())??;
    let owed = b"adopted by the child\nstill the parent's\n";
    if heard != owed {
        return Err(format!("the peer heard {:?}, not {:?}", String::from_utf8_lossy(&heard), String::from_utf8_lossy(owed)));
    }
    Ok(())
}

/// `test_rs_socket_export`: for the child and then the parent, a datagram
/// saying it is ready, answered by one saying go, and a connection through the
/// forwarded port that it accepts and writes its line on; then, once the parent
/// and a second child both say they are about to block, two connections, the
/// second once a racer wrote its line on the first, and two datagrams, each
/// taken once.
fn sockets_exported_to_a_child(qemu: &mut QemuInstance, forwarded: u16) -> Result<(), String> {
    use std::io::Read;
    /// A liveness ceiling on each of the peer's waits, never a timing.
    const CEILING: Duration = Duration::from_secs(120);
    let datagrams = std::net::UdpSocket::bind("127.0.0.1:0").map_err(|e| format!("no loopback datagram socket: {e}"))?;
    datagrams.set_read_timeout(Some(CEILING)).map_err(|e| e.to_string())?;
    let port = datagrams.local_addr().map_err(|e| format!("the datagram socket's port: {e}"))?.port();
    let heard = std::thread::spawn(move || -> Result<(), String> {
        for who in ["child", "parent"] {
            let mut ready = [0u8; 64];
            let (n, from) = datagrams.recv_from(&mut ready).map_err(|e| format!("the {who}'s datagram: {e}"))?;
            if &ready[..n] != format!("{who} ready").as_bytes() {
                return Err(format!("the peer heard {:?} for the {who}'s datagram", String::from_utf8_lossy(&ready[..n])));
            }
            datagrams.send_to(b"go", from).map_err(|e| format!("go to the {who}: {e}"))?;
            let mut conn = std::net::TcpStream::connect(("127.0.0.1", forwarded)).map_err(|e| format!("the connect for the {who}: {e}"))?;
            conn.set_read_timeout(Some(CEILING)).map_err(|e| e.to_string())?;
            let mut line = Vec::new();
            conn.read_to_end(&mut line).map_err(|e| format!("the {who}'s line, until its FIN: {e} after {line:?}"))?;
            let owed = format!("accepted by the {who}\n");
            if line != owed.as_bytes() {
                return Err(format!("the peer heard {:?}, not {owed:?}", String::from_utf8_lossy(&line)));
            }
        }
        let both = |what: &str| -> Result<std::net::SocketAddr, String> {
            let mut said = Vec::new();
            let mut from = None;
            for _ in 0..2 {
                let mut line = [0u8; 64];
                let (n, at) = datagrams.recv_from(&mut line).map_err(|e| format!("a racer's {what}: {e}"))?;
                said.push(String::from_utf8_lossy(&line[..n]).into_owned());
                from = Some(at);
            }
            said.sort();
            if said != [format!("parent {what}"), format!("racer {what}")] {
                return Err(format!("the racers said {said:?}"));
            }
            from.ok_or_else(|| "no racer spoke".to_string())
        };
        both("racing")?;
        for _ in 0..2 {
            let mut conn = std::net::TcpStream::connect(("127.0.0.1", forwarded)).map_err(|e| format!("a racer's connect: {e}"))?;
            conn.set_read_timeout(Some(CEILING)).map_err(|e| e.to_string())?;
            let mut line = Vec::new();
            conn.read_to_end(&mut line).map_err(|e| format!("a racer's line: {e} after {line:?}"))?;
            if line != b"accepted by a racer\n" {
                return Err(format!("a racer wrote {:?}", String::from_utf8_lossy(&line)));
            }
        }
        let socket = both("receiving")?;
        for datagram in [&b"one"[..], b"two"] {
            datagrams.send_to(datagram, socket).map_err(|e| format!("a racer's datagram: {e}"))?;
        }
        Ok(())
    });
    let result = qemu.run_test(&format!("test_rs_socket_export {port}"), Duration::from_secs(300));
    if let Some(why) = &result.error {
        return Err(format!("{why}\n{}\nthe job said:\n{}", peer_verdict(heard), result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    for claim in [
        "a listener and a datagram socket exported to a child: it accepted and spoke on them, and so did the parent after it",
        "two holders blocked on one queue: each took one connection and one datagram",
    ] {
        if !result.stdout.contains(claim) {
            return Err(format!("the job never said {claim:?}:\n{}", result.stdout));
        }
    }
    heard.join().map_err(|_| "the peer's thread panicked".to_string())?
}

/// What the peer's thread had come to when its job failed: a guest that waits
/// for a step the peer could not take is no stall of the guest's.
fn peer_verdict<T>(heard: std::thread::JoinHandle<Result<T, String>>) -> String {
    if !heard.is_finished() {
        return "the peer was still waiting on the guest".into();
    }
    match heard.join() {
        Ok(Ok(_)) => "the peer had done its part".into(),
        Ok(Err(why)) => format!("the peer failed first: {why}"),
        Err(_) => "the peer's thread panicked".into(),
    }
}

/// `test_rs_link_failover` on two virtio NICs: the harness, asked over the
/// second link's network, sets the first link's NIC down and then up.
fn traffic_leaves_by_the_second_link_while_the_first_is_down() -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    const JOB: &str = "link_failover";
    /// A liveness ceiling on the job and the peer's waits, never a timing.
    const CEILING: Duration = Duration::from_secs(300);
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, JOB);
    let config = compile::repo_root().join("tests/images/netcase.toml");
    let options = BootOptions { profile: qemu::Profile::HeadlessTwoNics, qmp: true, ..BootOptions::default() };
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[(JOB.to_string(), bin)], options);
    serial::Serial::boot(&qemu).must_be_clean()?;
    let peer = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("no loopback listener: {e}"))?;
    let port = peer.local_addr().map_err(|e| format!("the listener's port: {e}"))?.port();
    let socket = qemu.qmp_socket().to_path_buf();
    let heard = std::thread::spawn(move || -> Result<(), String> {
        let (conn, _) = peer.accept().map_err(|e| format!("the guest's connect: {e}"))?;
        conn.set_read_timeout(Some(CEILING)).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(conn.try_clone().map_err(|e| e.to_string())?);
        let mut answer = conn;
        let mut link = qemu::QmpLink::open(&socket);
        for word in ["down", "up"] {
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| format!("the job's {word:?} ask: {e}"))?;
            let Some((said, network)) = line.trim_end().split_once(' ') else { return Err(format!("the job asked {line:?}")) };
            if said != word {
                return Err(format!("the job asked {line:?} where {word:?} was owed"));
            }
            let Some((id, _)) = qemu::TWO_NICS.iter().find(|(_, net)| net.split('/').next() == Some(network)) else {
                return Err(format!("the job named {network:?}, which is neither NIC's network"));
            };
            link.set(id, word == "up");
            answer.write_all(format!("{word}\n").as_bytes()).map_err(|e| format!("the answer to {word:?}: {e}"))?;
        }
        Ok(())
    });
    let result = qemu.run_test(&format!("test_rs_{JOB} {port}"), CEILING);
    if let Some(why) = &result.error {
        return Err(format!("{why}\n{}\nthe job said:\n{}", peer_verdict(heard), result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    heard.join().map_err(|_| "the peer's thread panicked".to_string())??;
    for line in result.stdout.lines().filter(|l| l.contains("PASS: ")) {
        eprintln!("  [links] {}", line.trim());
    }
    Ok(())
}

/// `test_rs_link_restart` on `tests/images/linkcase.toml`'s boot: the peer
/// answers each line the job writes on one connection, the one before the
/// job ends its driver and the one after it starts another, then reads the
/// job's FIN and answers it with its own.
fn a_link_ends_under_a_held_stream_and_comes_back() -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    const JOB: &str = "link_restart";
    /// A liveness ceiling on the peer's waits, never a timing.
    const CEILING: Duration = Duration::from_secs(240);
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, JOB);
    let config = compile::repo_root().join("tests/images/linkcase.toml");
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let peer = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("no loopback listener: {e}"))?;
    let port = peer.local_addr().map_err(|e| format!("the listener's port: {e}"))?.port();
    let heard = std::thread::spawn(move || -> Result<(), String> {
        let (conn, _) = peer.accept().map_err(|e| format!("the guest's connect: {e}"))?;
        conn.set_read_timeout(Some(CEILING)).map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(conn.try_clone().map_err(|e| e.to_string())?);
        let mut answer = conn;
        for word in ["before", "after"] {
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| format!("the job's {word:?} line: {e}"))?;
            if line != format!("{word}\n") {
                return Err(format!("the peer heard {line:?} where the job's {word:?} line was owed"));
            }
            answer.write_all(format!("heard {word}\n").as_bytes()).map_err(|e| format!("the answer to {word:?}: {e}"))?;
        }
        let mut rest = String::new();
        match reader.read_line(&mut rest) {
            // The job's FIN, which this answers with its own as it drops.
            Ok(0) => Ok(()),
            Ok(_) => Err(format!("the peer heard {rest:?} after the job's last line")),
            Err(e) => Err(format!("the stream's FIN: {e}")),
        }
    });
    let result = qemu.run_test(&format!("test_rs_{JOB} {port}"), CEILING);
    if let Some(why) = &result.error {
        return Err(format!("{why}\n{}\nthe job said:\n{}", peer_verdict(heard), result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    heard.join().map_err(|_| "the peer's thread panicked".to_string())??;
    for line in result.stdout.lines().filter(|l| l.contains("PASS: ")) {
        eprintln!("  [link] {}", line.trim());
    }
    Ok(())
}

/// `test_rs_udp_recv_timeout` on `tests/images/netcase.toml`'s boot.
fn udp_receive_times_out() -> Result<(), String> {
    const JOB: &str = "udp_recv_timeout";
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, JOB);
    let config = compile::repo_root().join("tests/images/netcase.toml");
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_udp_recv_timeout", Duration::from_secs(120));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    Ok(())
}

/// `test_rs_netstack_connect_killed` on `netcase`'s real netstack
/// (`tests/guest/rust-tests/src/bin/netstack_connect_killed.rs`).
fn a_client_killed_mid_connect_gives_its_place_back() -> Result<(), String> {
    const JOB: &str = "netstack_connect_killed";
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, JOB);
    let config = compile::repo_root().join("tests/images/netcase.toml");
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_netstack_connect_killed", Duration::from_secs(120));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    if !result.stdout.contains("a client killed mid-connect gave its place back") {
        return Err(format!("the job never said its claim:\n{}", result.stdout));
    }
    Ok(())
}

/// `test_rs_stream_handles` on a boot of its own: a holder killed inside its
/// gate leaves the other working, netstack's exit ends a blocked read in each,
/// within a process a stream's handles are one end, one end reads and writes
/// on two threads, and piped children are served across threads
/// (`tests/guest/rust-tests/src/bin/stream_handles.rs`).
fn stream_holders(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "stream_handles";
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, JOB);
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_stream_handles", Duration::from_secs(300));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    for claim in [
        "a holder killed inside the gate: its hang-up came after it, and the parent wrote and read",
        "netstack's exit: a blocked read in each holder answered ConnectionAborted",
        "one command spawned twice: each child had a holder of its own, and both wrote",
        "given out and taken back: one end; a duplicate of its handle endowed or sent: refused",
        "one end, two threads: the write completes beside an unanswered read, which then takes the peer's next bytes",
        "piped children across threads: fed, read and waited for apart, and one killed beside its reader",
    ] {
        if !result.stdout.contains(claim) {
            return Err(format!("the job never said {claim:?}:\n{}", result.stdout));
        }
    }
    Ok(())
}

/// A claim's memory BAR asked for again — while an earlier answer is held, and
/// once its handle is gone, closed or never installed for want of room: each
/// time the kernel answers a handle whose mapping reads the function's own
/// register, two held at once map apart, and the job that asked ends on its
/// own.
fn bar_map_again(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "bar_map_again";
    let tests = compile::repo_root().join("tests/guest/rust-tests");
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, JOB);
    let msix = qemu::build_toyos_bin(qemu::SUITE_ARCH, &tests, MSIX_IN_PLACE);
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[(JOB.to_string(), bin), (MSIX_IN_PLACE.to_string(), msix)],
        BootOptions::default(),
    );
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
    let read = job_window(&mut qemu, &format!("test_rs_{MSIX_IN_PLACE}"))?;
    for line in msix_read_in_place(&read, &read)? {
        eprintln!("  [claims] {line}");
    }
    Ok(())
}

/// The job that reads each claimed function's armed MSI-X entry back through
/// its holder's mapping.
const MSIX_IN_PLACE: &str = "msix_in_place";

/// Every entry `msix_in_place` read through its holder's mapping is the one
/// the kernel logged writing, and its BAR was handed over where firmware put
/// it; at least one was read.
fn msix_read_in_place(kernel: &str, log: &str) -> Result<Vec<String>, String> {
    const READS: &str = ": entry 0 at BAR ";
    let mut judged = Vec::new();
    for line in log.lines().filter(|l| l.contains("msix_in_place: ") && l.contains(READS)) {
        let field = |key: &str| line.split_once(key).and_then(|(_, rest)| rest.split_whitespace().next());
        let (Some(at), Some(bar), Some(address), Some(data)) =
            (field(" at PCI "), field(READS), field(" reads address="), field(" data="))
        else {
            return Err(format!("an unreadable read: {line:?}"));
        };
        let at = at.trim_end_matches(':');
        let armed = format!("PCI {at}: msix address={address} data={data}");
        if !kernel.contains(&armed) {
            return Err(format!("{line:?} is not what the kernel armed: no `{armed}` in\n{kernel}"));
        }
        let handed = format!("pcidev: PCI {at} BAR {bar} (");
        if !kernel.lines().any(|l| l.contains(&handed) && l.contains(" is handed over where firmware put it, ")) {
            return Err(format!("{at}'s BAR {bar} was never handed over where firmware put it:\n{kernel}"));
        }
        judged.push(line.split_once("msix_in_place: ").map_or(line, |(_, rest)| rest).to_string());
    }
    if judged.is_empty() {
        return Err(format!("msix_in_place read no entry:\n{log}"));
    }
    Ok(judged)
}

/// The virtio NIC behind a root port is claimed and its registers read
/// through its BAR where firmware put it, inside the port's window.
fn bar_map_behind_a_bridge(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "bar_map_again";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let options = BootOptions { profile: qemu::Profile::HeadlessNicBehindPort, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], options);
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_bar_map_again", Duration::from_secs(60));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    let handed = "pcidev: PCI 01:00.0 BAR ";
    let Some(line) = result.serial.lines().find(|l| l.contains(handed) && l.contains(" is handed over where firmware put it, ")) else {
        return Err(format!("the NIC behind the port was never handed over a BAR:\n{}", result.serial));
    };
    eprintln!("  [claims] {}", line.trim());
    Ok(())
}

/// The kernel answers a lent window that is the virtio NIC's BAR as no memory
/// it allocated, by `fstat`, so no reader maps it as a region.
fn lent_device_memory_refused(test_config: &Path) -> Result<(), String> {
    const JOB: &str = "lent_device_memory_refused";
    const SAID: &str = "lent_device_memory_refused: a device's BAR lent as a window is not memory the kernel allocated";
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let mut qemu =
        QemuInstance::boot_with_options(test_config, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    serial::Serial::boot(&qemu).must_be_clean()?;
    let result = qemu.run_test("test_rs_lent_device_memory_refused", Duration::from_secs(60));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    let Some(line) = result.stdout.lines().find(|l| l.contains(SAID)) else {
        return Err(format!("the job never said `{SAID}`:\n{}", result.stdout));
    };
    eprintln!("  [claims] {}", line.trim());
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
    let read = common::irqcensus::windows_under(kernel.text(), cpus, &exited(WINDOWS_LOAD))?;
    eprintln!(
        "  [windows] held irqs_off_ns={} preempt_off_ns={}; herd irqs_off_ns={} preempt_off_ns={}",
        read.held.0, read.held.1, read.load.0, read.load.1
    );
    // Under the log write, each CPU's longest windows are judged against
    // the bench's record (`timings::ceiling`), the base they were first read at.
    boot.job_passed(LOG_WRITE)?;
    let reached = metal::bytes_of_lines(boot.log().text(), "log_write: line ");
    if reached < 1 << 20 {
        return Err(format!("{reached} bytes of the log write's lines reached the disk, under a mebibyte"));
    }
    let under = common::irqcensus::windows_under(kernel.text(), cpus, &exited(LOG_WRITE))?;
    eprintln!("  [windows] log write irqs_off_ns={} preempt_off_ns={}", under.load.0, under.load.1);
    boot.measured("boot.windowscase.log_write.irqs_off_ns", under.load.0)?;
    boot.measured("boot.windowscase.log_write.preempt_off_ns", under.load.1)
}

fn pci_cap_selftest(log: &str) -> Result<(), String> {
        if let Some(bad) = log.lines().find(|l| l.contains("pci cap selftest FAILED")) {
            return Err(format!("{bad}\n{log}"));
        }
        let Some(verdict) = log.lines().find(|l| l.contains("pci cap selftest")) else {
            return Err(format!("the walk's self-test never ran:\n{log}"));
        };
        // The count, not the absence of a FAILED line, which zero cases satisfy too.
        if !verdict.contains("selftest 6/6") {
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
    let unloaded = log.lines().filter(|l| l.contains("spawn: /tmp/no_such_program: ")).count();
    if unloaded != 2 {
        return Err(format!(
            "the loader refused /tmp/no_such_program {unloaded} times, not once per B"
        ));
    }
    let refused: Vec<&str> = log.lines().filter(|l| l.contains("spawn: refused under pid ")).collect();
    if refused.len() != 1 || !refused[0].contains("at depth 65, more than 64 below the supervisor") {
        return Err(format!("the kernel's depth refusals were {refused:?}, not one naming depth 65"));
    }
    Ok(())
}

/// `app_view` passed, every arm its package asks held, and the supervisor
/// refused each of the two launches for its own reason: a package named after
/// the shell's row, and one whose folder is a file. The guest sees only that
/// each was refused. And the supervisor wrote and took away the package's
/// grant, and refused the machine's session one.
fn app_view(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_app_view")?;
    let log = back.log();
    let named = format!("supervisor: launcher: {}", toyos_manifest::row_named("shell"));
    for said in [
        "  every arm held",
        "app_view: the app saw its own package read-only and its own folder as HOME, and nothing else",
        &named,
        "supervisor: launcher: appview was not started: /home/toy/Apps/appview could not be made: \
         /home/toy/Apps/appview is no directory",
        "supervisor: grants: appview is granted /home/toy/Games read-write",
        "  granted: cwd=/home/toy/Games roms=[\"test.gba\"] saved=Ok",
        "supervisor: grants: appview's grant is revoked; its next launch holds no folder",
    ] {
        if !log.text().lines().any(|l| l.contains(said)) {
            return Err(format!("the log never said `{said}`\n{}", log.text()));
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

/// Every unit this kernel went on to program, and every AMD-Vi unit it
/// switched off, said it was handed over with what the actuator left on; and
/// where every VT-d unit has protected memory regions, as the T14's do, they
/// went on before the hand-over and off after it.
///
/// An AMD-Vi unit's lines are on the log disk only because the switch-off
/// let that disk's DMA through the unit the actuator left blocking it.
fn iommu_firmware_left(log: &str) -> Result<(), String> {
        // VT-d unit lines alone: an AMD-Vi unit's read `iommu: amdvi unit`.
        let vtd: Vec<&str> = log.lines().filter(|l| l.contains("iommu: unit") && l.contains(" cap=")).collect();
        let guarded = !vtd.is_empty() && vtd.iter().all(|l| l.contains(" pmr=y"));
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
        let amdvi = log
            .lines()
            .filter_map(|l| l.split_once("iommu: amdvi ")?.1.split_once(" handed over control="))
            .map(|(unit, _)| unit);
        for unit in amdvi {
            for line in common::iommu::amdvi_left_on(unit) {
                if !log.contains(&line) {
                    return Err(format!(
                        "amdvi {unit} never said {line:?}, so the state the actuator left was \
                         never switched off:\n{log}"
                    ));
                }
            }
            let off = format!("iommu: amdvi {unit} @");
            if !log.lines().any(|l| l.contains(&off) && l.contains(" switched off control=")) {
                return Err(format!("amdvi {unit} was handed over and never said it was switched off:\n{log}"));
            }
            units += 1;
        }
        if units == 0 {
            return Err(format!("no unit was programmed or switched off, so nothing was handed over:\n{log}"));
        }
        let at = |needle: &str| log.lines().position(|l| l.contains(needle));
        let armed = at("iommu: protected memory on for");
        let first_off = at("was handed over with translation on; it goes off first");
        let released = at("iommu: protected memory released on");
        let first_translating = at(" translating gsts=");
        if guarded
            && !matches!((armed, first_off, released, first_translating),
                (Some(a), Some(o), Some(r), Some(t)) if a < o && r < t)
        {
            return Err(format!(
                "every unit has protected memory regions, and they did not go on before translation \
                 first went off and come off before the first unit was programmed:\n{log}"
            ));
        }
        let passed = log.matches("handed over with compatibility-format pass-through on").count();
        eprintln!(
            "  [iommu-firmware-left] {units} unit(s) switched off what they were handed over with, \
             {} protected memory; {passed} of them reported compatibility format passed",
            if guarded { "inside" } else { "without" }
        );
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

/// What the loader handed and what each CPU's `microcode:` line says: the
/// loader found its bench's patch where `images/microcode.toml`'s `benches`
/// put it, and one line from each CPU this machine brought up, all of one
/// signature, none refused, and each saying what a bench of that signature
/// is held to.
///
/// - **The T14**, 0x806c1 platform 7: every CPU runs 0xbe, the update Intel's
///   `06-8c-01` holds for it, whether it found it `current` or `loaded` it.
/// - **The AMD bench**, 0x860f01: every CPU runs 0x860010d, the patch AMD's
///   Family 17h container holds for it, and `loaded` it: its firmware loads
///   0x8600106, and a core's second thread writes the patch too, over the
///   level its first loaded.
fn microcode_on_metal(loader: &str, log: &str, cpus: u32) -> Result<(), String> {
    const T14: u32 = 0x0008_06c1;
    const AMD_BENCH: u32 = 0x0086_0f01;
    let lines = microcode_lines(log)?;
    let ids: BTreeSet<u32> = lines.iter().map(|line| line.cpu).collect();
    let want: BTreeSet<u32> = (0..cpus).collect();
    if ids != want || lines.len() != want.len() {
        return Err(format!("expected one microcode line from each of {want:?}, got {lines:?}"));
    }
    let signature = lines[0].signature;
    if let Some(other) = lines.iter().find(|line| line.signature != signature) {
        return Err(format!("cpu0 is signature {signature:#x} and {other:?} is not"));
    }
    let (handed, holds, held_to): (&str, fn(&MicrocodeSaid) -> bool, &str) = match signature {
        T14 => (
            "Microcode: /system/share/microcode/intel/06-8c-01/80, 112640 bytes at ",
            |said| matches!(said, MicrocodeSaid::Read { outcome, running: 0xbe } if ["current", "loaded"].contains(&outcome.as_str())),
            "every CPU runs 0xbe, which Intel's 06-8c-01 holds for it",
        ),
        AMD_BENCH => (
            "Microcode: /system/share/microcode/amd/17-60-01, 3200 bytes at ",
            |said| matches!(said, MicrocodeSaid::Read { outcome, running: 0x0860_010d } if outcome == "loaded"),
            "every CPU loaded 0x860010d, which AMD's Family 17h container holds for it",
        ),
        _ => return Err(format!("no bench of signature {signature:#x} says what its CPUs are held to")),
    };
    if !loader.contains(handed) {
        let said: Vec<&str> = loader.lines().filter(|l| l.contains("Microcode: ")).collect();
        return Err(format!("loader.log does not say {handed:?}; its microcode lines: {said:?}"));
    }
    if let Some(line) = lines.iter().find(|line| !holds(&line.said)) {
        return Err(format!("{line:?}: on signature {signature:#x} {held_to}"));
    }
    eprintln!("  [microcode] {} CPUs of signature {signature:#x}: {held_to}", lines.len());
    Ok(())
}

/// One `microcode:` line, the fields [`microcode_on_metal`] holds.
#[derive(Debug)]
struct MicrocodeLine {
    cpu: u32,
    signature: u32,
    said: MicrocodeSaid,
}

#[derive(Debug)]
enum MicrocodeSaid {
    /// It read its revision: `loaded`, `current`, `refused`, or `loads` for
    /// one that loads nothing, and the revision it runs.
    Read { outcome: String, running: u32 },
    /// It read nothing: its vendor has no loader.
    Unread,
}

/// Each bench's CPUID.01H:EAX, which its kernel's `microcode:` lines name it by.
const T14_SIGNATURE: u32 = 0x0008_06c1;
const AMD_BENCH_SIGNATURE: u32 = 0x0086_0f01;

/// Every `microcode:` line in `log`, or the first CPU that refused the update
/// it was handed, on any machine.
fn microcode_lines(log: &str) -> Result<Vec<MicrocodeLine>, String> {
    let mut out = Vec::new();
    for line in log.lines() {
        let Some(rest) = line.split("microcode: cpu").nth(1) else { continue };
        let words: Vec<&str> = rest.split_whitespace().collect();
        let field = |name: &str| words.iter().position(|w| *w == name).and_then(|at| words.get(at + 1));
        let hex = |name: &str| -> Option<u32> { u32::from_str_radix(field(name)?.trim_start_matches("0x"), 16).ok() };
        let unreadable = || format!("unreadable microcode line: {line:?}");
        let said = match words.iter().position(|w| *w == "found") {
            Some(found) => MicrocodeSaid::Read {
                outcome: words.get(found + 2).ok_or_else(unreadable)?.to_string(),
                running: hex("running").ok_or_else(unreadable)?,
            },
            None if rest.contains(" loads nothing: ") => MicrocodeSaid::Unread,
            None => return Err(unreadable()),
        };
        out.push(MicrocodeLine {
            cpu: words.first().and_then(|id| id.parse().ok()).ok_or_else(unreadable)?,
            signature: hex("signature").ok_or_else(unreadable)?,
            said,
        });
        if let Some(line @ MicrocodeLine { said: MicrocodeSaid::Read { outcome, running }, .. }) = out.last() {
            if outcome == "refused" {
                return Err(format!("cpu{} refused the update it was handed and runs {running:#x}: {line:?}", line.cpu));
            }
        }
    }
    Ok(out)
}

/// Every CPU's `CR0`, `CR4` and `XCR0`, against what a CPU running this
/// kernel must hold.
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
/// `XCR0` holds x87, SSE and AVX, and AVX-512's three components all or none
/// (SDM Vol. 1 §13.3), and nothing else: a component enabled and not declared
/// is state the kernel's save area may not hold.
///
/// Both halves, because the kernel writes every register whole: every bit named
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
        // Every kernel leaf is global, and only with `PGE` set is one cached so.
        (7, "PGE", true),
        (9, "OSFXSR", true),
        (10, "OSXMMEXCPT", true),
        (12, "LA57", false),
        // Clear: `CR4.FSGSBASE` gates RD/WR FS/GS BASE at every CPL (Intel SDM
        // Vol. 3A §2.5, Vol. 2 `WRGSBASE`), so no Ring 3 thread aims `GS.base`.
        (16, "FSGSBASE", false),
        // Set: user programs use AVX, whose state only `XSAVE` moves.
        (18, "OSXSAVE", true),
        // Not a bit the machine may withhold: `Arch::cpu`'s two x86-64 CPUs
        // are the only x86-64 CPUs this repository launches and both name
        // `+smep`, so a boot without supervisor-mode execution prevention is a
        // kernel that stopped enabling it or a launcher that stopped asking.
        (20, "SMEP", true),
    ];
    /// The `CR4` bits the CPU may withhold, so neither answer is wrong.
    const CR4_MAY: &[(u32, &str)] = &[(11, "UMIP"), (17, "PCIDE"), (21, "SMAP")];
    const XCR0_BITS: &[(u32, &str, bool)] = &[(0, "x87", true), (1, "SSE", true), (2, "AVX", true)];
    /// AVX-512's components, which a CPU without AVX-512 does not have.
    const XCR0_AVX512: &[(u32, &str)] = &[(5, "opmask"), (6, "ZMM_Hi256"), (7, "Hi16_ZMM")];

    let mut seen: Vec<(u32, u64, u64, u64)> = Vec::new();
    // The save form each CPU names, which says which path this machine runs;
    // the kernel asserts they agree.
    let mut forms: BTreeSet<&str> = BTreeSet::new();
    for line in log.lines() {
        let Some(rest) = line.split("control_regs: cpu").nth(1) else { continue };
        let Some((id, rest)) = rest.split_once(" cr0=0x") else { continue };
        let Some((cr0, rest)) = rest.split_once(" cr4=0x") else { continue };
        let word = |key: &str| {
            rest.split_whitespace()
                .find_map(|w| w.strip_prefix(key))
                .and_then(|v| u64::from_str_radix(v, 16).ok())
        };
        let (Ok(id), Ok(cr0), Ok(cr4), Some(xcr0), Some(form)) = (
            id.parse::<u32>(),
            u64::from_str_radix(cr0, 16),
            u64::from_str_radix(rest.split_whitespace().next().unwrap_or(""), 16),
            word("xcr0=0x"),
            rest.split_whitespace().find_map(|w| w.strip_prefix("save=")),
        ) else {
            return Err(format!("unreadable control-register line: {line:?}"));
        };
        seen.push((id, cr0, cr4, xcr0));
        forms.insert(form);
    }

    // Which CPUs answered, not how many lines were printed: a boot where one AP
    // never came up at all must not pass by having the BSP print twice.
    let ids: BTreeSet<u32> = seen.iter().map(|&(id, _, _, _)| id).collect();
    let want: BTreeSet<u32> = (0..cpus).collect();
    if ids != want {
        return Err(format!(
            "expected one line from each of {want:?}, got {ids:?}:\n{log}"
        ));
    }

    // A bit named nowhere above is as much of the declaration as a named one,
    // and the kernel writes both registers whole — so `UMIP`, `TSD` or
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
    for &(id, cr0, cr4, xcr0) in &seen {
        let mut wrong = String::new();
        for (reg, value, bits, known) in [
            ("cr0", cr0, CR0_BITS, named(CR0_BITS, &[])),
            ("cr4", cr4, CR4_BITS, named(CR4_BITS, CR4_MAY)),
            ("xcr0", xcr0, XCR0_BITS, named(XCR0_BITS, XCR0_AVX512)),
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
        let avx512 = XCR0_AVX512.iter().filter(|&&(bit, _)| xcr0 & (1 << bit) != 0).count();
        if avx512 != 0 && avx512 != XCR0_AVX512.len() {
            wrong += " xcr0 holds part of AVX-512's components, which it takes all or none";
        }
        if !wrong.is_empty() {
            return Err(format!("cpu{id} cr0={cr0:#010x} cr4={cr4:#010x} xcr0={xcr0:#x}:{wrong}"));
        }
    }

    // A CPU that agrees about every bit named above can still differ in one
    // that is not, and a thread migrating onto it would execute differently
    // from one moment to the next.
    let (_, cr0, cr4, xcr0) = seen[0];
    if let Some(&(id, other0, other4, other_x)) =
        seen.iter().find(|&&(_, a, b, x)| (a, b, x) != (cr0, cr4, xcr0))
    {
        return Err(format!(
            "cpu0 has cr0={cr0:#010x} cr4={cr4:#010x} xcr0={xcr0:#x} and cpu{id} has \
             cr0={other0:#010x} cr4={other4:#010x} xcr0={other_x:#x}"
        ));
    }

    eprintln!("  [control_regs] {cpus} CPUs, cr0={cr0:#010x} cr4={cr4:#010x} xcr0={xcr0:#x} save={forms:?}");
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
/// declared at boot, `pm_enable=1`, and its power envelope in every read the
/// one its `control_regs:` line holds. No line, the kernel's or a
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
/// span: MPERF counts at a fixed rate, the TSC's, and APERF at the actual
/// clock (SDM Vol. 3B §22.7.2, 325462-093); and every CPU's APERF ran at
/// least as fast as its MPERF across the spin, its busy clock at or above the
/// rate MPERF counts at (2419.2 MHz on the T14). A spin under a request whose
/// maximum is `Highest_Performance` falls below it only when a power limit or
/// firmware holds the CPU there.
///
/// Read and not held: each CPU's idle busy fraction, which an SMI in the idle
/// second raises on every CPU alike by the time it held them, its spinning
/// clock, the package's power across the idle second and the spin, what one
/// round cost its reader, and how late each CPU's kick handler
/// ran under the `loaded` phase, which
/// `issues/toyos-beats-linuxs-latency-on-the-t14.md` sets its bar for.
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
            for counter in
                ["smi", "aperf", "mperf", "hwp_request", "hwp_request_pkg", "energy_perf_bias", "package_energy", "core_energy"]
            {
                if !reads.contains(&format!("{counter}={}", counters.contains_key(counter))) {
                    return Err(format!("{name}: cpu{cpu} carries {counters:?} and its bring-up said {reads:?}"));
                }
            }
            if counters.get("stale") != Some(&0) || counters.get("hardware_id").map(u64::to_string) != Some(roster[*cpu].clone()) {
                return Err(format!("{name}: cpu{cpu} is stale or not lapic {}: {counters:?}", roster[*cpu]));
            }
            let declared = kernel.must_say(&format!("control_regs: cpu{cpu} pm_enable=1 "))?;
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
        // The energy counters are 32 bits wide and wrap (`toyos_abi::counters`).
        for (name, &value) in idle1[&cpu].iter().filter(|(name, _)| !name.ends_with("_energy")) {
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
    log.must_say("counters_metal loaded: ")?;
    for said in log.text().lines().filter(|l| l.contains("counters_metal loaded: ")) {
        eprintln!("  [counters] {}", said.split("counters_metal ").nth(1).unwrap_or(said).trim());
    }
    let aperf_per_mperf: Vec<f64> = (0..cpus).map(|cpu| ratio(idle1, spin, cpu, "aperf", "mperf")).collect();
    eprintln!(
        "  [counters] {cpus} cpus, the SMI count on each moved by {asked} over {} ms, the commands the kernel wrote to SMI_CMD in it; TSC {tsc_mhz:.0} MHz",
        (at2 - at0) / 1_000_000
    );
    for (cpu, busy) in busy.iter().enumerate() {
        eprintln!(
            "  [counters] cpu{cpu}: idle busy {:.2}%, spinning {:.0} MHz, busy {busy:.3}",
            ratio(idle0, idle1, cpu, "mperf", "stamp") * 100.0,
            tsc_mhz * aperf_per_mperf[cpu],
        );
    }
    eprintln!("  [counters] the spin's read, a whole round, took its reader {} ns", clock["spin"].1);
    if let (Some(&unit), Some(_)) = (idle0[&0].get("rapl_power_unit"), idle0[&0].get("package_energy")) {
        use osbench_rules::energy;
        let mw = |a: &Read, b: &Read, from: u64, to: u64| {
            energy::span(a[&0]["package_energy"], b[&0]["package_energy"], energy::RANGE)
                .map(|steps| energy::milliwatts(energy::microjoules(steps, unit), to - from))
                .map_or("unreadable".to_string(), |mw| mw.to_string())
        };
        eprintln!(
            "  [counters] the package drew {} mW across the idle second and {} mW across the spin (a reading)",
            mw(idle0, idle1, at0, at1),
            mw(idle1, spin, at1, at2),
        );
    }
    if aperf_per_mperf.iter().any(|&r| r < 1.0) {
        return Err(format!("APERF ran {aperf_per_mperf:.3?} of MPERF across the spin, some cpu's busy clock below the rate MPERF counts at"));
    }
    Ok(())
}

/// The T14's ACPI row as the kernel filled it from the machine's FACP and
/// APIC, each value the field ACPI 6.5 Tables 5.9, 5.10, 5.25 and 5.26 give
/// the machine's bytes (`lib/acpi/tests/thinkpad_t14.rs`); the server armed on
/// it; the embedded controller at the ports and GPE the T14's ECDT names (ACPI
/// 6.5 §5.2.15), as ToyOS's own boots there read it; at least one of its
/// queries taken and a count of them logged, and no guard of the server's
/// fired.
fn acpi_events_on_metal(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_acpi_counted")?;
    back.job_passed("test_rs_acpi_hold")?;
    let (log, kernel) = (back.log(), back.kernel());
    let enabled = kernel.must_say("acpi: ACPI mode: ACPI_ENABLE 0xf0 written to SMI_CMD 0xb2 ")?;
    smi_cmd_writer(enabled)?;
    eprintln!("  [acpi] {}", enabled.trim());
    kernel.must_say(
        "acpi: the ACPI row: PM1a events 0x1800+4, GPE0 0x1860+32, SCI gsi 9 level/high, the \
         fixed-hardware power button; the firmware handed over in legacy mode",
    )?;
    log.must_say("acpiserver: armed: power button served")?;
    log.must_say(&format!("{}0x6e at 0x66/0x62", acpiserver_api::CONTROLLER_SERVED))?;
    let lines: Vec<&str> = log
        .text()
        .lines()
        .filter(|l| toyos_logstream::program_line(l).is_some_and(|said| said.tag == "acpiserver"))
        .collect();
    if let Some(fired) = lines.iter().find(|l| l.contains("panicked")) {
        return Err(format!("the server died: {fired}"));
    }
    if let Some(refused) = lines.iter().find(|l| l.contains("method was refused, so the GPE is served no more")) {
        return Err(format!("a runtime GPE's method was refused: {refused}"));
    }
    for gpe in lines.iter().filter(|l| l.contains("acpiserver: GPE ") && l.contains(" taken for the first time; its method ran")) {
        eprintln!("  [acpi] {}", gpe.trim());
    }
    let firsts: Vec<&&str> = lines.iter().filter(|l| l.contains("query") && l.contains("taken for the first time")).collect();
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

/// What one bench's tables are, as the check outside the tree read them on
/// their dump and the bench's own boots on this row (#875) found them.
struct Bench {
    /// The kernel's line for the ACPI row the FADT names, to its SCI.
    row: &'static str,
    /// How many definition blocks the XSDT and the FADT name.
    blocks: usize,
    /// Each block refused, by its place and its refusal's kind: a firmware
    /// defect the tracker records.
    refused: &'static [(usize, &'static str)],
    /// The kernel's line for the sleep type the server handed it, whole to
    /// the value, read some way that is not this server's evaluation.
    s5: &'static str,
    /// The blocks the processors' `_OSC` load under ToyOS's claims, by
    /// signature and length.
    loaded: &'static [&'static str],
    /// The end of the server's line for the processors' idle states: what
    /// every declaration it handed over held, and what it withheld.
    idle: &'static str,
    /// Each register the kernel keeps that this machine's AML writes: the
    /// server says it once, and its evaluation goes on.
    kept: &'static [At],
    /// Pages of registers this machine's AML writes, each of which the
    /// server says the kernel's first write into.
    first_written: &'static [At],
}

/// Where a write the server says lands: at an address, or at an offset into
/// the 128 KiB MCHBAR window (631122 §3.1.14), wherever 00:00.0 places it.
#[derive(Clone, Copy, Debug)]
enum At {
    Address(u64),
    Mchbar(u64),
}

impl At {
    fn holds(self, address: u64) -> bool {
        match self {
            At::Address(at) => address == at,
            At::Mchbar(offset) => address & 0x1_FFFF == offset,
        }
    }
}

/// The address of each write the server said under `head`, which it details
/// on the next line, under [`acpiserver_api::OWN`], as `... to Memory(<hex>)`.
fn said_writes(lines: &[&str], head: &str) -> Result<Vec<u64>, String> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.contains(head))
        .map(|(i, l)| {
            let own = lines.get(i + 1).filter(|next| next.contains(acpiserver_api::OWN)).ok_or_else(|| format!("{l:?} is followed by no line of the machine's own"))?;
            u64::from_str_radix(field_between(own, " to Memory(", ")")?, 16).map_err(|_| format!("{own:?} names no address"))
        })
        .collect()
}

/// The T14: 14 blocks; its `\_S5`'s `SLP_TYPa` read by a byte scan of its
/// DSDT, with no interpreter; the first processor's commit of ToyOS's claim
/// (bits 4 and 9) loads three blocks, its `_CST`'s among them, and every
/// processor's `_CST` then names `MWAIT 0x31` and `0x60` beyond C1. Its controller's `_INI` writes its GPIO pad
/// `PAD_CFG_DW0_GPPC_A_13` at 0xFD6E0A70 (631120-002 §27.1.114), and its
/// hotkey device's `_INI` writes MCHBAR's cTDP control and turbo activation
/// ratio, at 5F50h and 5F54h, and PL1 at 59A0h, which the kernel keeps
/// (`issues/the-t14s-thermal-modes-do-not-move-its-package-power-limits.md`).
const T14: Bench = Bench {
    row: "acpi: the ACPI row: PM1a events 0x1800+4, GPE0 0x1860+32, SCI gsi 9 level/high",
    blocks: 14,
    refused: &[],
    s5: "power: S5 is PM1a 0x1804 with SLP_TYPa=7,",
    loaded: &["SSDT, 916 bytes", "SSDT, 459 bytes", "SSDT, 1162 bytes"],
    idle: " with MWAIT 0x31 253 µs + MWAIT 0x60 1048 µs; withheld: none; unread: none",
    kept: &[At::Mchbar(0x59A0)],
    first_written: &[At::Address(0xFD6E_0000), At::Mchbar(0x5000)],
};

/// The AMD bench: 17 blocks; its SSDT8 ends in a `Scope` of an `EC0` no
/// table defines, refused whole as the owner ruled
/// (`issues/the-amd-benchs-ssdt8-scopes-a-controller-no-table-defines.md`);
/// its `\_S5`'s `SLP_TYPa` as the check outside the tree evaluated it; no
/// processor names an `_OSC`; each `_CST` hands over its port 0x414 and its
/// type-3 0x415, a Family 17h part's (`system/acpiserver/src/cst.rs`). Its
/// controller's `_REG(3, 1)` and its
/// adapter's `_PSR` each reach `ALIB`, whose mailbox writes the root
/// complex's 0xB8 and 0xBC through ECAM, in 00:00.0's page at 0xF8000000.
const AMD: Bench = Bench {
    row: "acpi: the ACPI row: PM1a events 0x400+4, GPE0 0x420+8, SCI gsi 9 level/low",
    blocks: 17,
    refused: &[(9, "a name that does not resolve")],
    s5: "with SLP_TYPa=5,",
    loaded: &[],
    idle: " with port 0x414 400 µs + port 0x415 350 µs; withheld: none; unread: none",
    kept: &[],
    first_written: &[At::Address(0xF800_0000)],
};

/// The server's load of a bench's tables, every access the kernel's to make
/// for it, read in the log up to the server's power sources line, which the
/// boot was held for: each of the machine's definition blocks, fetched through `SYS_ACPI`, summing to zero as its firmware sealed
/// it, and loaded or refused as its [`Bench`] says; `\_S5`'s `SLP_TYPa`
/// handed to the kernel, which says it powers this machine off with that
/// value, and never does on this row. Then the namespace initialised (ACPI
/// 6.5 §6.5.1): every `_STA` and `_INI` the walk runs, none refused, and the
/// controller's `_REG(3, 1)` after them; each register the kernel keeps that
/// the bench's AML writes said once, and each page of registers it writes
/// said with the kernel's first write into it; every battery reading names
/// the adapter online or offline; `\_SB._OSC`
/// negotiated, and on an Intel machine each processor's committed, none
/// refused; every block they load loaded, none refused; every processor's
/// idle states the bench's, and none refused by the kernel. Every other CPU of the machine said
/// how its range registers stand beside the boot processor's, and none holds
/// registers that are on and not those. What it prints beside that is the
/// first measurement of each: the load's time, the reads by address space,
/// the takes that found the firmware holding the lock and its longest hold,
/// the pages of memory by type, the longest wait an evaluation asked, the
/// GPEs enabled, the interpreter's heap and the namespace's slots.
fn acpi_tables_on_metal(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_acpi_hold")?;
    let (whole, kernel) = (back.log(), back.kernel());
    let text = whole.text();
    let at = text.find(acpiserver_api::POWER_SOURCES).ok_or("the server never said its power sources, which the hold waited for")?;
    let end = text[at..].find('\n').map_or(text.len(), |n| at + n);
    let log = serial::Serial::named("the log up to the server's power sources", &text[..end]);
    let lines: Vec<&str> = log
        .text()
        .lines()
        .filter(|l| toyos_logstream::program_line(l).is_some_and(|said| said.tag == "acpiserver"))
        .collect();
    if let Some(fired) = lines.iter().find(|l| l.contains("panicked")) {
        return Err(format!("the server died: {fired}"));
    }
    let bench = [T14, AMD].into_iter().find(|bench| kernel.text().contains(bench.row)).ok_or("no bench of this row's names its ACPI row so")?;
    let said: Vec<&str> = lines.iter().filter_map(|l| l.split_once("acpiserver: table ").map(|(_, said)| said.trim())).filter(|said| said.contains(" of ")).collect();
    let expected: Vec<String> = (1..=bench.blocks)
        .map(|place| {
            let name = if place == 1 { "DSDT" } else { "SSDT" };
            match bench.refused.iter().find(|(at, _)| *at == place) {
                Some((_, kind)) => format!("{place} of {} ({name}) refused: {kind}", bench.blocks),
                None => format!("{place} of {} ({name}) loaded", bench.blocks),
            }
        })
        .collect();
    if said != expected {
        return Err(format!("the server's tables are {said:#?}, where this bench's are {expected:#?}"));
    }
    let handed = log.must_say("acpiserver: \\_S5 handed to the kernel: ")?;
    let kept = kernel.must_say("power: S5 is PM1a ")?;
    if !kept.contains(bench.s5) {
        return Err(format!("the kernel keeps {kept:?}, where this bench's \\_S5 is {:?}", bench.s5));
    }
    eprintln!("  [acpi] {}; {}", handed.trim(), kept.trim());
    let others = number_between(kernel.text(), "SMP: ", " of ")? - 1;
    let compared: Vec<&str> = kernel.text().lines().filter(|l| l.contains("mtrr: cpu") && l.contains("'s range registers are ")).collect();
    if compared.len() as u64 != others || compared.iter().any(|l| l.contains("are on and not the boot processor's")) {
        return Err(format!(
            "{others} other CPUs came up, and their range registers beside the boot processor's are {compared:#?}"
        ));
    }
    let off = compared.iter().filter(|l| l.contains("range registers are off")).count();
    eprintln!("  [acpi] {others} other CPUs' range registers compared with the boot processor's: {off} off, none on and different");
    let refused: Vec<&&str> = lines
        .iter()
        .filter(|l| l.contains("acpiserver: refused") || l.contains(" refused: ") || l.contains("was refused for the first time"))
        .filter(|l| !l.contains("acpiserver: table "))
        .collect();
    if !refused.is_empty() {
        return Err(format!("the server refused something of this machine's AML: {refused:#?}"));
    }
    let kept = said_writes(&lines, acpiserver_api::KEPT_WRITE)?;
    let unmatched = |expected: &[At], said: &[u64]| expected.iter().copied().filter(|at| !said.iter().any(|&address| at.holds(address))).collect::<Vec<At>>();
    if kept.len() != bench.kept.len() || !unmatched(bench.kept, &kept).is_empty() {
        return Err(format!("the server said the kernel kept writes to {kept:#x?}, where this bench's AML writes {:x?}", bench.kept));
    }
    let pages: Vec<u64> = said_writes(&lines, acpiserver_api::FIRST_WRITE_IN_PAGE)?.iter().map(|address| address & !0xFFF).collect();
    let missing = unmatched(bench.first_written, &pages);
    if !missing.is_empty() {
        return Err(format!("the server said no first write into {missing:x?}; it said {pages:#x?}"));
    }
    eprintln!("  [acpi] writes the kernel kept: {kept:#x?}; pages first written: {pages:#x?}");
    let census = log.must_say("acpiserver: the namespace initialised: ")?;
    let refusals = census.split_once("evaluations refused, by kind: ").map_or("", |(_, kinds)| kinds.trim());
    if refusals != "none" {
        return Err(format!("the namespace's initialisation refused an evaluation: {census}"));
    }
    if !census.contains("_INI run, _REG(3, 1) in 1 scope(s); ") {
        return Err(format!("the census says the controller's _REG(3, 1) ran in no one scope: {census}"));
    }
    let readings: Vec<&&str> = lines.iter().filter(|l| l.contains(acpiserver_api::BATTERY_READ)).collect();
    if readings.is_empty() || readings.iter().any(|l| !(l.ends_with("; AC online") || l.ends_with("; AC offline"))) {
        return Err(format!("a battery reading names the adapter neither online nor offline: {readings:#?}"));
    }
    log.must_say("acpiserver: \\_SB._OSC negotiated: ")?;
    if bench.loaded.is_empty() {
        if let Some(processors) = lines.iter().find(|l| l.contains("processors' _OSC")) {
            return Err(format!("this bench's processors name no _OSC: {processors}"));
        }
    } else {
        let processors = log.must_say("acpiserver: the processors' _OSC committed, Intel's: ")?;
        if !processors.contains(", 0 refused;") {
            return Err(format!("a processor's _OSC was refused: {processors}"));
        }
        eprintln!("  [acpi] {}", processors.trim());
    }
    let idle = log.must_say(acpiserver_api::IDLE_CENSUS)?;
    if !idle.contains(" 0 refused by the kernel;") || !idle.trim_end().ends_with(bench.idle) {
        return Err(format!("the processors' idle states are not this bench's, or the kernel refused one: {idle}"));
    }
    eprintln!("  [acpi] {}", idle.trim());
    let loads: Vec<&str> = lines.iter().filter_map(|l| l.split_once("acpiserver: a table loaded at run time: ").map(|(_, said)| said.trim())).collect();
    if loads != bench.loaded {
        return Err(format!("the tables loaded at run time are {loads:?}, where this bench's claims load {:?}", bench.loaded));
    }
    let blocks = bench.blocks - bench.refused.len();
    let took = log.must_say(&format!("acpiserver: {blocks} of {} tables loaded in ", bench.blocks))?;
    let bytes = log.must_say("acpiserver: the tables' bytes took ")?;
    let aml = log.must_say("acpiserver: the tables' AML read SystemMemory ")?;
    let memory = number_between(aml, "AML read SystemMemory ", " times, SystemIO ")?;
    let takes = number_between(aml, "; took the Global Lock ", " times, ")?;
    if bench.blocks == T14.blocks && (memory == 0 || takes == 0) {
        return Err(format!("this machine's tables read memory and take the Global Lock as they load, and the server's did not: {aml}"));
    }
    let size = log.must_say(" table(s) loaded at run time and 0 refused; the namespace holds ")?;
    let gpes = log.must_say("acpiserver: GPEs: ")?;
    for line in [took, bytes, aml, census, size, gpes] {
        eprintln!("  [acpi] {}", line.trim());
    }
    Ok(())
}

/// The end of the kernel's line for each of the AMD bench's CPUs, its `_CST`
/// kept whole.
const AMD_KEPT_STATES: &str = "waits in C1 or port 0x414 (exit 400 µs, residency 800 µs), port 0x415 (exit 350 µs, residency 700 µs)";

/// The AMD bench's idle states kept on every CPU, and each CPU in one across
/// an idle second: once the server's census is in the log, `idle_entered`
/// reads each CPU's deep idle periods across the second, those the read
/// ending it woke the CPU from included. Held: the kernel's line for every CPU
/// the bring-up started names [`AMD_KEPT_STATES`], each CPU's reads fresh,
/// and each CPU chose a declared state at least once in the second. Every such period is a
/// request of port 0x415: the kernel chooses the deepest state whose exit and
/// residency fit and whose history admits it (`kernel::sched::idle::choose`),
/// and 0x415's exit and residency are both below 0x414's. What the core then
/// entered no counter on this part says. The AMD bench alone: the T14 has not
/// run it.
fn idle_entered_on_metal(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_idle_entered")?;
    let kernel = back.kernel();
    if !kernel.text().contains(AMD.row) {
        return Err("this row runs on the AMD bench, and this machine's ACPI row is not that bench's".into());
    }
    let cpus = back.cpus()?;
    for cpu in 0..cpus {
        let kept = kernel.must_say(&format!("idle: cpu{cpu} (processor UID "))?;
        if !kept.trim_end().ends_with(AMD_KEPT_STATES) {
            return Err(format!("cpu{cpu} keeps {kept:?}, where the bench declares {AMD_KEPT_STATES:?}"));
        }
    }
    let log = back.log();
    let entered: Vec<&str> = log
        .text()
        .lines()
        .filter_map(|l| toyos_logstream::program_line(l)?.text.strip_prefix("idle_entered: ").map(str::trim))
        .collect();
    if entered.len() != cpus as usize {
        return Err(format!("idle_entered read {} CPUs, and the bring-up started {cpus}: {entered:#?}", entered.len()));
    }
    for (cpu, said) in entered.iter().enumerate() {
        let periods = number_between(said, &format!("cpu{cpu} "), " deep idle periods")?;
        if periods == 0 || said.ends_with(", stale") {
            return Err(format!("cpu{cpu} chose no declared state across the idle second, or was read stale: {said}"));
        }
        eprintln!("  [idle] {said}");
    }
    eprintln!("  [idle] every CPU {AMD_KEPT_STATES}");
    Ok(())
}

/// What the T14's tables name, by a census of them read outside the tree:
/// one control-method battery, one AC adapter, and one embedded controller,
/// attached to the server's AML.
const T14_POWER_SOURCES: &str = "acpiserver: power sources: 1 of 1 control-method batteries present, 1 AC adapter(s), with an embedded controller attached; the Global Lock taken ";

/// The T14's battery as the server read it: the one battery its tables name
/// found present once the controller's `_REG` ran, its information read,
/// and at least one reading of it with the adapter's state; nothing refused.
/// The census's Global Lock counts are printed and held to nothing: an
/// evaluation gives back the lock it still holds as it ends
/// (`finish`, `system/acpiserver/aml/src/exec.rs`), and the real lock
/// word's exchange is `acpi_tables_on_metal`'s to hold. The lines are printed
/// whole.
fn battery_on_metal(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_acpi_hold")?;
    let log = back.log();
    let lines: Vec<&str> = log
        .text()
        .lines()
        .filter(|l| toyos_logstream::program_line(l).is_some_and(|said| said.tag == "acpiserver"))
        .collect();
    if let Some(fired) = lines.iter().find(|l| l.contains("panicked")) {
        return Err(format!("the server died: {fired}"));
    }
    let refused: Vec<&&str> = lines.iter().filter(|l| l.contains("acpiserver: refused") || l.contains(" refused: ")).collect();
    if !refused.is_empty() {
        return Err(format!("the server refused something of this machine's AML: {refused:#?}"));
    }
    let found = log.must_say(T14_POWER_SOURCES)?;
    let info = log.must_say(&format!("acpiserver: {}battery 1 of 1 from ", acpiserver_api::BATTERY_INFO))?;
    let read: Vec<&&str> = lines.iter().filter(|l| l.contains(acpiserver_api::BATTERY_READ)).collect();
    if read.is_empty() {
        return Err("the server said no reading of the battery".into());
    }
    if let Some(unknown) = read.iter().find(|l| l.contains("unknown") || !(l.ends_with("; AC online") || l.ends_with("; AC offline"))) {
        return Err(format!("a reading with a value unknown: {unknown}"));
    }
    for line in [found, info].into_iter().chain(read.iter().map(|l| **l)) {
        eprintln!("  [battery] {}", line.trim());
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
/// FADT names through the direct map and found the lock word in memory the
/// firmware's own map does not report as `AddressRangeMemory`, which ACPI 6.5
/// §5.2.10 forbids the FACS.
fn acpi_death_on_metal(back: &metal::Readback) -> Result<(), String> {
    back.job_passed("test_rs_acpi_release")?;
    let kernel = back.kernel();
    let lock = kernel.must_say("acpi: the Global Lock is the FACS's at ")?.trim();
    let uefi_type = lock
        .rsplit_once(", in memory the firmware's map types ")
        .and_then(|(_, ty)| ty.parse::<u8>().ok())
        .ok_or_else(|| format!("the Global Lock's line names no memory type: {lock}"))?;
    if ADDRESS_RANGE_MEMORY.contains(&uefi_type) {
        return Err(format!("the Global Lock's word is in AddressRangeMemory: {lock}"));
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

/// The UEFI memory types ACPI 6.5 Table 15.6 maps to `AddressRangeMemory`.
const ADDRESS_RANGE_MEMORY: [u8; 5] = [1, 2, 3, 4, 7];

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
/// control-register declaration, and every AP's timestamp counter is bounded
/// against the BSP's.
///
/// The kernel bounds each AP's offset with round trips of a cache line as it
/// starts and panics on one they prove, so the row reads each AP's bound and
/// prints the widest. **QEMU cannot refute it**: every guest TSC is
/// synthesised from one host clock, which is why it runs on metal.
fn smp_roster_and_tsc_trail(log: &str, cpus: u32) -> Result<(), String> {
    clock_kept_its_counter(log)?;
    let online = number_between(log, "SMP: ", " of ")?;
    let named = number_between(log, " of ", " MADT cpus online")?;
    if online != u64::from(cpus) || named != u64::from(cpus) {
        return Err(format!(
            "the machine was given {cpus} CPUs, its MADT named {named} and {online} came up"
        ));
    }
    let bounds: Vec<(i64, i64)> = log.lines().filter_map(tsc_offset_bound).collect();
    let aps = u64::from(cpus) - 1;
    if bounds.len() as u64 != aps {
        return Err(format!("{} of {aps} APs' TSCs were bounded against the BSP's", bounds.len()));
    }
    let checked = number_between(log, "control_regs: ", " of ")?;
    if checked != u64::from(cpus) {
        return Err(format!("{checked} of {cpus} CPUs were checked against the declaration"));
    }
    let widest = bounds.iter().map(|&(lo, hi)| lo.unsigned_abs().max(hi.unsigned_abs())).max().unwrap_or_default();
    eprintln!("  [smp] {online} CPUs online, every AP's TSC within {widest} cycles of the BSP's");
    Ok(())
}

/// `lo..=hi` off `clock: cpuN's TSC is within lo..=hi cycles of cpu0's`.
fn tsc_offset_bound(line: &str) -> Option<(i64, i64)> {
    let (_, rest) = line.split_once("'s TSC is within ")?;
    let (bound, _) = rest.split_once(" cycles of cpu0's")?;
    let (lo, hi) = bound.split_once("..=")?;
    Some((lo.parse().ok()?, hi.parse().ok()?))
}

/// The local APIC mode a bench's CPUs offer, by `CPUID.01H:ECX[21]` as each
/// bench's scout read it on every CPU (`kernel/sim/tests/scout/`): the T14's
/// `0x77fafbbf` sets it and the AMD bench's `0x76d8320b` does not, so the
/// declaration keeps x2APIC on the one and xAPIC on the other.
fn apic_mode_offered(log: &str) -> Result<&'static str, String> {
    let signature =
        microcode_lines(log)?.first().ok_or("the kernel wrote no microcode line to tell the bench by")?.signature;
    match signature {
        T14_SIGNATURE => Ok("X2apic"),
        AMD_BENCH_SIGNATURE => Ok("Xapic"),
        other => Err(format!("no bench of signature {other:#x} has a recorded APIC mode")),
    }
}

/// Each CPU's `topo:` line against its bench's tree as the bench's own scout
/// read it (`kernel/sim/tests/scout/`, decoded by the simulator's
/// `a_benchs_shape_is_what_its_scout_read`), the bench told by its CPU
/// signature. The line is the CPUID the kernel read live; the expectation is
/// words another kernel logged.
///
/// - **T14**: x2APIC ids `0, 2, 4, 6, 1, 3, 5, 7` in MADT order, a core per
///   id pair and one L3 for the package.
/// - **AMD bench**: APIC ids `0..16`, a core per id pair, an L3 per eight ids
///   and one package.
fn topology_readback(log: &str, cpus: u32) -> Result<(), String> {
    let signature =
        microcode_lines(log)?.first().ok_or("the kernel wrote no microcode line to tell the bench by")?.signature;
    const AMD_IDS: [u32; 16] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    let (bench, ids, smt, llc, package): (&str, &[u32], u32, u32, u32) = match signature {
        T14_SIGNATURE => ("the T14", &[0, 2, 4, 6, 1, 3, 5, 7], 1, 4, 4),
        AMD_BENCH_SIGNATURE => ("the AMD bench", &AMD_IDS, 1, 3, 7),
        other => return Err(format!("no bench of signature {other:#x} has a recorded tree")),
    };
    if cpus as usize > ids.len() {
        return Err(format!("{cpus} CPUs came up and {bench}'s MADT names {}", ids.len()));
    }
    for (cpu, id) in ids.iter().take(cpus as usize).enumerate() {
        let want = format!("topo: cpu{cpu} core={:#x} llc={:#x} pkg={:#x}", id >> smt, id >> llc, id >> package);
        if !log.lines().any(|l| l.ends_with(&want)) {
            let said: Vec<&str> = log.lines().filter(|l| l.contains("topo: ")).collect();
            return Err(format!("{bench}: no `{want}` record; the kernel said {said:#?}"));
        }
    }
    let tree = log.lines().find(|l| l.contains(&format!("topo: {cpus} cpus in "))).ok_or("no `topo:` summary")?;
    eprintln!("  [topology] {bench}: {}", tree.split("topo: ").nth(1).unwrap_or(tree));
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
        ("MCFG", "ACPI: ECAM window: "),
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

/// The LAPIC timer calibrated to a frequency.
fn timer_calibration(log: &str) -> Result<(), String> {
    let lapic_hz = number_between(log, "ticks/10ms, so ", "Hz")?;
    if lapic_hz == 0 {
        return Err("the LAPIC timer calibrated to no frequency at all".to_string());
    }
    eprintln!("  [timer] LAPIC {lapic_hz}Hz");
    Ok(())
}

/// The rate the boot started the clock at: CPUID 15H's, or one the HPET
/// measured with its bound, which on the bench is a rate the clock can keep at
/// all. Its bound is held against minutes by [`clock_holds_over_minutes`].
fn tsc_rate_at_boot(log: &str) -> Result<(), String> {
    clock_kept_its_counter(log)?;
    let boot = boot_rate(log)?;
    if boot.millihertz == 0 {
        return Err("the TSC was started at no rate at all".to_string());
    }
    eprintln!("  [timer] TSC {boot:?}");
    Ok(())
}

/// The rate the clock started at, in millihertz, and its bound in ppb: zero
/// for CPUID 15H's, which is not measured.
#[derive(Debug)]
struct BootRate {
    millihertz: u64,
    bound_ppb: u64,
    stated: bool,
}

fn boot_rate(log: &str) -> Result<BootRate, String> {
    if let Ok(hz) = number_between(log, "clock: the TSC counts at ", " Hz, as CPUID 15H states") {
        return Ok(BootRate { millihertz: hz * 1_000, bound_ppb: 0, stated: true });
    }
    let millihertz = number_between(log, "clock: the TSC counts at ", " mHz against the HPET over ")?;
    let bound_ppb = number_between(log, " ms, within ", " ppb; CPUID 15H states no rate")?;
    Ok(BootRate { millihertz, bound_ppb, stated: false })
}

/// One of the clock's reports: the TSC's rate against the HPET over a span
/// since boot and its bound, and the rate the clock keeps after it.
#[derive(Debug)]
struct ClockReport {
    span_s: u64,
    measured_millihertz: u64,
    measured_ppb: u64,
    kept_millihertz: u64,
}

fn clock_report(line: &str) -> Option<ClockReport> {
    let (_, rest) = line.split_once("clock: over ")?;
    let (span, rest) = rest.split_once(" s the TSC counted ")?;
    let (measured, rest) = rest.split_once(" mHz against the HPET, within ")?;
    let (measured_ppb, rest) = rest.split_once(" ppb; the clock ")?;
    let kept = rest.split(' ').find_map(|word| word.parse().ok())?;
    Some(ClockReport {
        span_s: span.parse().ok()?,
        measured_millihertz: measured.parse().ok()?,
        measured_ppb: measured_ppb.parse().ok()?,
        kept_millihertz: kept,
    })
}

/// Whether the boot's clock kept the TSC: every step it slewed, every span it
/// left out and every step it waited on the RTC for is printed, and a
/// demotion fails the row, since a demoted boot's processes read the HPET.
fn clock_kept_its_counter(log: &str) -> Result<(), String> {
    const SAID: [&str; 4] = ["clock: TSC STEP: ", "so this span is left out", "so the span was left out", "the RTC's edges after it will say which moved"];
    for line in log.lines().filter(|line| SAID.iter().any(|said| line.contains(said))) {
        eprintln!("  [clock] {}", line.trim());
    }
    match log.lines().find(|line| line.contains("clock: DEMOTED")) {
        Some(demoted) => Err(format!("the clock demoted the TSC: {}", demoted.trim())),
        None => Ok(()),
    }
}

/// Parts per billion `a` is from `b`.
fn ppb_apart(a: u64, b: u64) -> u64 {
    (u128::from(a.abs_diff(b)) * 1_000_000_000 / u128::from(b)) as u64
}

/// **The clock over minutes.** The kernel holds the TSC to the HPET once a
/// second, panics on a drift and demotes the TSC on a step it does not slew, so
/// a boot that ran its list to the end without a demotion held; what this row
/// reads is whether the rate the boot started at was within the bound it
/// derived, against the rate the HPET measured over
/// the longest span the boot reached, which must be two minutes or more. A
/// stated rate is CPUID 15H's, and against it the HPET's own 0.05% is all a
/// document allows (IA-PC HPET Specification 1.0a, §2.2), so its distance is
/// printed.
fn clock_holds_over_minutes(boot: &metal::Readback) -> Result<(), String> {
    let kernel = boot.kernel();
    let text = kernel.text();
    clock_kept_its_counter(text)?;
    let started = boot_rate(text)?;
    let longest = text
        .lines()
        .filter_map(clock_report)
        .max_by_key(|report| report.span_s)
        .ok_or("the clock never reported its rate against the HPET")?;
    if longest.span_s < 120 {
        return Err(format!("the clock's longest report spans {} s, under two minutes: {longest:?}", longest.span_s));
    }
    let apart = ppb_apart(started.millihertz, longest.measured_millihertz);
    eprintln!(
        "  [clock] started at {started:?}; over {} s the HPET measured {} mHz within {} ppb, {apart} ppb from the start; keeps {} mHz",
        longest.span_s, longest.measured_millihertz, longest.measured_ppb, longest.kept_millihertz,
    );
    let allowed = if started.stated { 500_000 } else { started.bound_ppb } + longest.measured_ppb;
    if apart > allowed {
        return Err(format!(
            "the clock started {apart} ppb from the rate the HPET measured over {} s, past the {allowed} ppb its bounds allow",
            longest.span_s
        ));
    }
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

/// What `wake_onto_busy`'s round trip may take at its 99th percentile: the
/// partner's wake served within microseconds by an idle CPU, against the
/// spinner's quantum a wake left on the busy CPU waits out every round.
const WAKE_ONTO_BUSY_P99_US: u64 = 1_000;

/// `wake_onto_busy`'s exit code is its p99 in microseconds, or a refusal.
fn wake_onto_busy(boot: &metal::Readback) -> Result<(), String> {
    for said in boot.log().text().lines().filter(|l| l.contains("wake_onto_busy: ")) {
        eprintln!("  [wake] {}", said.trim());
    }
    let code = boot.exit_code("test_rs_wake_onto_busy")?;
    let p99 = u64::try_from(code).map_err(|_| {
        format!(
            "wake_onto_busy exited {code}, which is a refusal and not a measurement; its own \
             last line in the boot's log says which"
        )
    })?;
    if p99 >= WAKE_ONTO_BUSY_P99_US {
        return Err(format!(
            "a round trip's p99 is {p99} µs, at or past {WAKE_ONTO_BUSY_P99_US} µs: a wake onto \
             a busy CPU is waiting for its quantum rather than for the idle one"
        ));
    }
    Ok(())
}

/// `job_fairness` judges its own shares and exits on the verdict; its line is
/// the measurement.
fn job_fairness(boot: &metal::Readback) -> Result<(), String> {
    for said in boot.log().text().lines().filter(|l| l.contains("job_fairness: ")) {
        eprintln!("  [jobs] {}", said.trim());
    }
    boot.job_passed("test_rs_job_fairness")
}

fn run_debug_mode(c_tests: &[(String, Vec<u8>)], rust_bins: &[(String, Vec<u8>)]) {
    let cmd_path = Path::new("/tmp/toyos-debug-cmd");
    let result_path = Path::new("/tmp/toyos-debug-result");
    let ready_path = Path::new("/tmp/toyos-debug-ready");

    let _ = fs::remove_file(cmd_path);
    let _ = fs::remove_file(result_path);
    let _ = fs::remove_file(ready_path);

    let test_config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images/testcases.toml");
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
    let rust_tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/guest/rust-tests");
    let mut rust_bins = qemu::build_toyos_bins(&rust_tests_dir);
    rust_bins.push(crypto_parity_unasserted());
    let c_names = discover_c_tests();
    eprintln!(
        "[toyos] Compiling {} C tests, and attempting {} declared ones...",
        c_names.len(),
        NOT_RUN.len()
    );
    check_not_run();
    (compile_c_tests(&c_names), rust_bins)
}

/// `crypto_parity` with debug assertions off, by the name its job runs.
fn crypto_parity_unasserted() -> (String, Vec<u8>) {
    let dir = compile::repo_root().join("tests/guest/rust-tests");
    let bin = qemu::build_toyos_bin_as(qemu::SUITE_ARCH, &dir, "crypto_parity", toyos_build::build::UNASSERTED);
    ("crypto_parity_unasserted".to_string(), bin)
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

/// Machine tests a run boots only when a filter names them, so no unfiltered
/// run, CI's included, boots one: each needs a product only
/// `cargo run -- --hosted-clang` builds, which no CI job holds.
const ON_DEMAND: &[&str] = &["hosted_clang_hello"];

/// Whether a run takes `name`: a filter matches it, or there is none and it is
/// not [`ON_DEMAND`].
fn kept(filters: &[&str], name: &str) -> bool {
    filters.iter().any(|f| name.contains(f)) || (filters.is_empty() && !ON_DEMAND.contains(&name))
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
            let at = compile::repo_root().join(arm.config);
            if !at.is_file() {
                return Err(format!("{name} boots {}, which is no image config", arm.config));
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
        arms: &[metal::once("nowhere", "tests/images/no-such-config.toml", &[], &[])],
        judge,
    };
    /// A fixture's name, its METAL rows, and the refusal it must draw, `None`
    /// for none.
    type Case = (&'static str, &'static [(&'static str, metal::Metal)], Option<&'static str>);
    let cases: &[Case] = &[
        ("two rows", &[("one", RUNS), ("two", RUNS)], None),
        ("a row twice", &[("one", RUNS), ("one", RUNS)], Some("has two metal declarations")),
        ("no boot", &[("one", NONE)], Some("asks for no boot at all")),
        ("no config", &[("one", NO_CONFIG)], Some("which is no image config")),
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

/// What the shared boots answer for: every discovered Rust binary, the T14's
/// [`LAST_MEMBERS`] and every corpus case.
fn shared_names() -> Vec<String> {
    let metal = LAST_MEMBERS.iter().map(ToString::to_string);
    discover_rust_tests().into_iter().chain(metal).chain(discover_c_tests()).collect()
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
    let parsed = match toyos_metal::testargs::parse(&args) {
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
        let boots = shared_metal(&c_bins);
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
            match metal::run(mode, parsed.metal_host, &selected, &boots, &rust_bins, !nocapture && !debug_mode)
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

    let test_config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images/testcases.toml");
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
