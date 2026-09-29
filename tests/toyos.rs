mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use common::qemu::{
    self, await_guest, await_marker, await_marker_new, BootOptions, QemuInstance, TestResult,
    STALLED, TIMED_OUT,
};
use common::{
    audio, compile, devices, faults, lan, metal, partclaim, pkg, power, screen, serial, storage,
    usb,
};
use toyos_build::bootlog::{self, boot_millis};
use toyos_build::testargs::{self, Shard, SUITE};
use toyos_build::redlist;
use toyos_build::tiers::{Reach, Schedule, Tier};

struct TestDef {
    name: String,
    qemu_name: String,
    timeout: Duration,
    check: fn(&TestResult) -> bool,
    /// What the test's window is still owed when the guest's exit closed it.
    ///
    /// A capture ends at `===TEST_END===`, which is the test process exiting —
    /// and a daemon reporting *on* that exit writes its line afterwards. So a
    /// check that counts such lines is counting over a window its own subject
    /// closes, and the last one loses the race
    /// (`null_sink_client_exits`, PR #85 and PR #94). This runs between the
    /// test and its check, with the guest still up, and waits on the guest's
    /// own liveness — never on a span of host time. [`no_settle`] is the
    /// default and costs nothing.
    settle: fn(&mut QemuInstance, &mut TestResult),
}

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

/// Which tier the shared boot's discovered members are in: one boot, so one
/// tier. Declared beside [`SHARED_BLOCK`] rather than assumed, for the same
/// reason that is declared.
const SHARED_TIER: Tier = Tier::Fast;

/// The one boot that carries every Rust and C test.
///
/// Declared here for the same reason each list entry is: it is a scheduling
/// answer and it has to be visible.
///
/// **Parallel, once its ceilings stopped being host-wide.** It was moved to the
/// tail because at width 4 `allocator_stress` went from 1 s to past its 5 s and
/// `demand_paging_sse` past its — but not one of those numbers is an assertion.
/// They are liveness guards on a guest that might wedge, and the verdict in
/// every case is the exit code and the expected stdout. [`qemu::budget`] now
/// pays them out per guest the phase may have up, which is what
/// `wait_for_ready`'s boot timeout has done since the phase existed, so the
/// number each author reasoned about is still the number for one guest.
///
/// What that leaves is one boot's worth of tests costing about thirteen seconds
/// between them, which is far too little to be worth a tail slot of its own:
/// alone it is thirteen seconds nothing overlaps, and in the phase it is one
/// task among sixty. The count is deliberately not written here — discovery is
/// what decides it and a comment restating it is wrong the next time somebody
/// adds a file.
const SHARED_BLOCK: Sched = Sched::Parallel;

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

/// How many times a shared block will answer a dead guest with a new one.
///
/// Bounded because a block whose every member kills the guest must not boot one
/// per test; three because the failure it exists for is one test taking the boot
/// down, and a block that does it four times has a different problem.
const MAX_SHARED_REBOOTS: usize = 3;

// Rust helper binaries that are spawned by tests, not tests themselves.
const RUST_SKIP: &[&str] = &[
    // blockd's supervisor and client: every role needs the second NVMe
    // controller only its own boots carry, and with no role it refuses by name.
    // `blockd_serves_partitions`, `blockd_survives_its_death`,
    // `blockd_dma_outside_the_lent` and `blockd_lends_within_its_bound` run it.
    "blockd_io",
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
    // **It reboots the machine**, so in the shared block it would end the boot
    // under whichever member came next; and its verdict is the order of the
    // console after that reset, which only its own boot holds.
    // `quiesce_stops_the_machine` runs it.
    "quiesce_writers",
    // The same, and its verdict is where one kernel line lands among others.
    // `quiesce_refuses_a_second_shutdown` runs it.
    "quiesce_twice",
    // The same, and its verdict is the stop record of a boot staged around it.
    "quiesce_last",
    // The same, and its verdict is the log volume the stop leaves.
    // `quiesce_leaves_the_volume_whole` runs it.
    "quiesce_fsync",
    // Its verdict is a count of what reached `/log`, which only a boot of its own
    // holds, and megabytes of it. `log_program_flood` runs it.
    "log_flood",
    // It exits 7 on purpose; its verdict is which exit a judge of `/log` reads.
    // `log_program_forgery` runs it.
    "log_forger",
    // Its verdict is where its one line went — `/log`, the served log and the
    // console — which only a boot of its own reads back.
    "log_origin",
    // Its verdict is where its line lands among the kernel's records, which
    // every other binary's records would crowd. `log_program_line_after_its_records`
    // runs it.
    "log_hold",
    // It prints init's word accepting a swap of netd; its verdict is that a
    // `logd` serving the network changed nothing. `log_carrier_forgery` runs it.
    "log_carrier_forger",
    // It asks for a stop its boot's kernel refuses; its verdict is its line
    // after that in `/log`. `log_after_a_refused_stop` and
    // `log_resume_meets_its_flush` run it.
    "log_refused_stop",
    // It waits for a cue only a kernel armed with `copy-meets-a-remap` gives.
    // `user_copy_races_munmap` runs it.
    "copy_out_races_munmap",
    // Only a kernel armed with `tls-rebase-window` holds a spawn in the window it probes.
    // `tls_rebase_window` runs it.
    "tls_dtv_race",
    // The C corpus's comparator: a helper reached through one symlink per case,
    // never a test of its own. `shared_metal` stages every name on this list.
    "ccheck",
    "disk_backtrace_child",
    "fault_gate_child",
    // `gsbase_locked`'s probe child; its #UD must kill the child, not the run.
    "gsbase_probe",
    "test_panic_child",
    // It takes the machine down; `panic_halts_the_others_first` runs it.
    "panic_halts_first",
    // A binary that panics at once, sent over ssh as a service's replacement;
    // `swap_crash_rolls_back` stages it from the host and never runs it as a job.
    "swap_crash",
    // The swap DMA control's replacement netd: it claims the 82574, stops it
    // and masters it. `swap_quiets_the_function` stages it.
    "swap_claim_idle",
    // Both holders of the residue isolation test: it claims the 82574 through
    // the test estate's capability, which only the E1000e profile has.
    // `userdev_residue_is_its_own` stages it.
    "userdev_residue",
    // The residue control's replacement netd: it masters the 82574 with its
    // receive unit as netd left it. `swap_keeps_what_nothing_reset` stages it.
    "swap_claim_running",
    // The refusal control's replacement netd: it aims the 82574's receive ring
    // outside its grant and waits on its claim. `swap_fault_tells_its_holder`
    // stages it.
    "swap_claim_astray",
    // The reset control's replacement netd: it claims the `igb` an Express
    // function level reset released. `swap_resets_the_function` stages it.
    "swap_flr_probe",
    // Run over ssh undeclared, it says whether it inherited the swap port.
    // `swap_not_inherited` uploads it.
    "swap_probe",
    "i8042_keyboard",
    "i8042_mouse",
    "input_events",
    // Meaningful only on `MetalNoUsb`, where no input source exists; on every
    // other machine both claims succeed. `input_claim_absent` runs it.
    "input_absent",
    // Needs a display whose mode can change, which is `Profile::VirtioGpu`
    // alone; the shared boot has no display at all. `gpu_set_resolution` runs
    // it there, and `iommu_gpu_scanout_swap` the second.
    "gpu_set_resolution",
    "gpu_scanout_swap",
    "va_exhaustion",
    // Needs a NIC in front of netd, a host serving TLS behind it and a CA
    // minted for that boot. `https_tls13` stages all three.
    "https_fetch",
    // Needs a NIC in front of netd; only `tests/netcase` has one.
    // `netd_listener_forgery` runs it there.
    "netd_listener_forgery",
    // Needs a NIC in front of netd and a host server behind it.
    // `netd_slow_reader`, `netd_held_open`, `netd_udp_refused`,
    // `netd_udp_any_address`, `netd_refused_pipes` and `netd_refused_accept`
    // run them on `tests/netcase`, and `netd_lookup_let_go` on it with its
    // frames held.
    "netd_slow_reader",
    "netd_held_open",
    "netd_udp_refused",
    "netd_udp_any_address",
    "netd_refused_pipes",
    "netd_refused_accept",
    "netd_lookup_let_go",
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
    // Needs SYS_DEBUG, which the shipping kernel has no arm of at all.
    // `heap_ceiling_bounds` boots the `test-actuators` kernel for it.
    "heap_ceiling",
    // Fills /tmp to the VFS listing limit, so it needs a boot nothing else
    // shares — every later `read_dir("/tmp")` in it would be refused.
    // `readdir_bound` gives it one.
    "readdir_bound",
    // Fills the VFS `created_dirs` cap and leaves it there. `mkdir_cap` runs it.
    "mkdir_cap",
    // Needs a live compositor, which `tests/testcases` does not boot.
    // `metal_sim_window_caps` runs it on the config that does.
    "window_caps",
    // Same reason, same config: `metal_sim_ipc_hostile_peer` runs it.
    "ipc_hostile_peer",
    // Same again: `metal_sim_compositor_stall` runs it.
    "compositor_stall",
    // Same again, and it spawns copies of itself as clients that die:
    // `metal_sim_client_death` runs it.
    "compositor_client_death",
    // Same again, and it also needs a host injecting pointer packets:
    // `metal_sim_window_drag` runs it.
    "window_drag",
    // Same again, and it also needs a host typing GUI+V:
    // `metal_sim_hostile_clipboard` runs it.
    "compositor_hostile_clipboard",
    // Needs a compositor, a terminal and a shell: `desktop_window_child`
    // launches it from that shell.
    "window_child",
    // Same again: the `toolkit_` tests of their names launch them from the
    // toolkit desktop.
    "window_wake",
    "winit_loop",
    "winit_pace",
    // Its two spawning arms only mean anything when the two processes share a
    // CPU, and the shared boot has two. `fpu_isolation` gives it a machine with
    // one — and a second boot on the kernel that saves nothing, which is the
    // only thing that proves the arms have teeth.
    "fpu_isolation",
    // Its host driver boots two kernels to compare, so a bare run says nothing.
    "gsbase_locked",
    // Needs netd with a NIC. `netd_connection_caps` runs it on tests/netcase.
    "netd_caps",
    // Need every owner `inspect` reads, which only tests/inspectcase runs.
    // `inspect_reads_its_owners` runs both there.
    "inspect_denied",
    "inventory_bounds",
    // Same reason, same config: `netd_hostile_peer` runs it there.
    "netd_hostile_peer",
    // Needs a `launcher` connector, which `tests/testcases`'s test-runner has
    // no reason to hold. `launcher_refusals` runs it on tests/netcase, whose
    // test-runner receives one for exactly this.
    "launcher_refusals",
    // Needs a launcher to tell its two roads apart, and a declared shell and
    // toybox to take it: `spawn_cwd` runs it on tests/netcase.
    "spawn_cwd",
    // Needs a boot image the harness staged a file into before the machine
    // started, which only `esp_filesystem` builds.
    "esp_files",
    // Every question it asks has the *right* answer on an ordinary kernel, so
    // on the shared boot it prints three successes and passes on its exit code
    // — a second test of the same name whose verdict is vacuous.
    // `boot_volume_metadata_error` runs it on the kernel that refuses the
    // reads, which is the only build it says anything about.
    "boot_volume_metadata_error",
    // Two modes, each waiting to be typed at through QMP; on its own nothing
    // ever answers it. `swiss_german_layout`, `locale_detect` and
    // `locale_detect_unrecognized` drive it.
    "locale_gate",
    // A victim, not a test: it spins on `SYS_GETPID` so that another CPU's NMIs
    // have somewhere to land, and on its own it asserts nothing and costs ten
    // seconds. `syscall_window_nmi` runs it on the kernel that storms it.
    "nmi_window_spin",
    // A victim, not a test: the load `dump-in-blocking-pass` files Ctrl+Alt+D
    // inside, and on its own it asserts nothing. `dump_left_pending_is_owed` runs
    // it on the kernel that stages it.
    "dump_stage_load",
    // Driven, not run: `screen_console_clear` types its name at a console it is
    // watching, and on its own it asks the kernel to paint over a panel nobody
    // is reading and exits 0. A verdict its own exit code cannot carry — the
    // same shape as `test_screen_churn` below, and it was in the shared registry
    // for the same reason nobody had looked.
    "test_screen_graffiti",
    // A workload, not a test: it prints a pattern for `screen_console_scroll`
    // to assert a panel against, and on its own it has no verdict at all. It
    // used to sit in the shared boot with defaults for its arguments, where it
    // printed four hundred lines to a console nothing was reading and passed
    // on its exit code.
    "test_screen_churn",
    // Spawns `/system/bin/doom` and reads the WAD, which `tests/testcases` does
    // not carry. `doom_frames` runs it on `tests/doommusiccase`.
    "doom_frames",
    // Its failure mode is a CPU that never runs anything again, so on the
    // shared boot it would be reported against whichever test came next — and
    // every one after that. `short_sleep_livelock` gives it a boot of its own.
    "abuse_short_sleep",
    // `cache_eviction` needs the small NVMe that makes the cache evict at all.
    "cache_eviction",
    // `writeback_reopen` and `writeback_spawn` each need their own boot with
    // `writeback-stall` armed; `writeback_durability` writes `/log` and is judged
    // host-side off the image after a shutdown. All three run as `MACHINE_TESTS`,
    // not on the shared boot.
    "writeback_reopen",
    "writeback_spawn",
    "writeback_durability",
    // Same shape as `writeback_durability`: what it stages on `/log` — a file
    // unlinked out from under a held descriptor, its clusters handed to the next
    // writer — is only half the claim, and the other half is the volume read
    // back off the image after a shutdown by a FAT implementation that is not
    // the kernel's. `fat_backing_revoked` runs it.
    "fat_backing_revoked",
    // Stages a rename with an absent source on `/log` and leaves the
    // destination for `fs_rename_durable` to read back off the image.
    "fs_rename_durable",
    // Stages real directories on `/log` for `fs_dirs_durable` to read back
    // off the image.
    "fs_dirs_durable",
    // Audio is judged on the T14 and nowhere else: the `hda_client_stall`,
    // `hda_tone`, `audio_idle_suspend`, `shipped_client_departures` and
    // `soundd_log_stall` metal rows run these.
    "hda_client_stall",
    "audio_tone",
    "audio_idle_suspend",
    "null_sink_client_exits",
    "soundd_log_stall",
    // Its whole subject is a page of a file the host wrote onto the volume
    // before the machine existed; the shared boot stages nothing, so it prints
    // `did not open` and passes on its exit code. `log_backing_read_error`
    // stages the file and reads the verdict.
    "log_volume_reread",
    // Needs `usb-flush-fails` armed: on the shared boot the device flush
    // succeeds and its two must-refuse assertions red for an honest reason.
    // `fsync_failed_commit` boots it with the arm.
    "fsync_flush_failed",
    // Needs the NVMe `/home` and a boot of its own for the readback it is judged against; `home_overwrite_reads_back` runs it.
    "home_overwrite_zero",
    // Needs a boot where the DATA volume is ours and absent; on the shared
    // boot `/apps` and `/home` are ordinarily mounted, so every refusal it
    // checks would be granted instead. `broken_data_volume_is_absent` and
    // `data_candidate_with_bad_geometry_is_absent` run it.
    "home_absent",
    // Needs the disks `tests/common/partclaim.rs` crafts, the boot stick's GUIDs
    // as arguments and a role; `partition_claim`, `partition_claim_gives_up` and
    // `partition_claim_departure` boot it and judge it off the images.
    "partition_claimant",
    // Needs `test-small-caches` for the eviction its read-back rests on, and a
    // boot of its own for the host-side re-read. `redirty_mid_flush` runs it.
    "redirty_mid_flush",
    // Needs `ftruncate-flush-stall` and a boot of its own for the host-side
    // re-read. `ftruncate_flush_race` runs it.
    "ftruncate_flush_race",
    // Needs the `smp-skip-ap` boot; `smp_failed_ap_leaves_no_hole` runs it there.
    "smp_hole_shootdown",
    // Its listings are exact against `tests/layoutcase`, and it takes what that
    // boot wrote as its argv. `layout_fresh_boot` runs it over ssh.
    "layout_paths",
    // Needs a package installed under `/apps` and a config whose `[apps]` row
    // is what a launch out of it holds; `pkg_install_gbae` gives both, and on
    // any other boot this exits on a launch nothing could satisfy.
    "pkg_launch_gbae",
];

/// Binaries a machine test drives that the shared boot also runs on purpose.
///
/// **A binary a machine test drives under a different name is still discovered
/// by [`discover_rust_tests`]**, still runs on the shared boot, and there
/// passes on its exit code with nothing staged for it to act on. `RUST_SKIP` is
/// one answer to that; this list is the other, for the binaries whose shared
/// run asserts something of its own. Every driven name is on one list or the
/// other, so neither answer is silence — `suite_split` is the gate.
/// `sched_stress` is the one whose two runs differ by *kernel* rather than by
/// what the host staged: the shipping build here, `sched_check_build`'s
/// assert-carrying build there.
#[allow(dead_code, reason = "`suite_split` reads it, in `toyos-checks` alone")]
const DRIVEN_AND_SHARED: &[&str] = &[
    // Its shared run is the x86-64 verdict; `virt_readonly_copyout` builds it
    // for AArch64 and runs it on that architecture's job case.
    "abuse_readonly_copyout",
    // The lost-wake canary: its shared run is the count on the shipping
    // kernel with nothing staged, and `blocking_read_window` drives it again
    // with the watch's window held open.
    "blocking_read_stress",
    // The log-stream arms drive it for the kernel's `exit:` record about it,
    // not for anything it does: it is the cheapest process this tree starts.
    "empty_dir_stat",
    // Its shared run judges `/tmp`'s and `/log`'s stamps; its other modes are machine tests'.
    "file_mtime",
    "hierarchy_paths",
    "nvme_home_roundtrip",
    "sched_stress",
    "std_alloc",
    "std_mmap",
    "wall_clock_now",
];

/// What `test-early-panic` panics with (`kernel/src/main.rs`): the last line its
/// report puts on serial.
const EARLY_PANIC_MESSAGE: &str = "test-early-panic: on-screen console check";

// Tests that read a decoded screendump, which is exactly the set for which
// the screen is the device under test: the panic console. On a machine with
// no serial port the rendered report is the only diagnostic that exists, so
// asserting on pixels there is asserting on the product. Everything else that
// used to read a screendump now reads the console instead — a screenshot is a
// poor way to ask "did the right process come up", and thresholds over a live
// desktop are how those tests passed vacuously twice.
/// The order was once about kernel rebuilds — every actuator was a build, and a
/// feature-carrying test last left the plain-kernel ones above it untouched by
/// the thrash. There are two kernels now and nothing to thrash; the order is
/// kept because these are read the way they are
/// written.
const SCREEN_TESTS: &[(&str, Sched, Tier)] = &[
    // Two boots, each ended at a loader line rather than at the kernel's ready
    // marker. Every verdict is a count of rows against a count of lines off the
    // same boot's console; no clock is in either.
    ("screen_loader_lines", Sched::Parallel, Tier::Nightly),
    ("screen_gop_firmware_mode", Sched::Parallel, Tier::Weekly),
    // The log is still on the panel once every program the image starts has
    // run: an event, and no clock in it.
    ("screen_diag_boot", Sched::Parallel, Tier::Nightly),
    // A guest halted in the window, so the panel is read where only the repaint
    // under test can have painted it.
    ("screen_early_panel", Sched::Parallel, Tier::Nightly),
    ("screen_log_absent", Sched::Parallel, Tier::Fast),
    ("screen_console_shell", Sched::Parallel, Tier::Fast),
    ("screen_console_clear", Sched::Parallel, Tier::Fast),
    ("screen_console_scroll", Sched::Parallel, Tier::Fast),
    ("screen_i8042_health", Sched::Parallel, Tier::Weekly),
    // Ctrl+Alt+D with no console at all: the panel is the whole channel, and a
    // compositor is holding it. The verdict is the report on the panel.
    ("screen_blocked_dump", Sched::Parallel, Tier::Nightly),
    ("screen_late_panic", Sched::Parallel, Tier::Fast),
    ("screen_paged_scrollback", Sched::Parallel, Tier::Weekly),
    ("screen_panic_muted", Sched::Parallel, Tier::Weekly),
    ("screen_console_panic", Sched::Parallel, Tier::Fast),
    ("screen_fatal_halt", Sched::Parallel, Tier::Fast),
    // The same fatal path from inside Ctrl+Alt+D's report painter, holding the
    // panel's latch it will never give back: the report has to take the screen
    // anyway, and its CPU has to go on to watch the reset bound.
    ("screen_fatal_behind_a_painter", Sched::Parallel, Tier::Nightly),
    // The same fatal path with a compositor holding the panel, which is the
    // only configuration the owner's laptop is ever in and the one no screen
    // test covered: `screen_fatal_halt` boots a config with no compositor, and
    // `screen_blocked_dump` has one but paints through `paint_report` rather
    // than through `halt_all_cpus`.
    ("screen_fatal_halt_composited", Sched::Parallel, Tier::Nightly),
    // Every PageUp moves the page one back, which the unattended deadline
    // never does: order, and no clock in it.
    ("screen_pager_keys", Sched::Parallel, Tier::Nightly),
    // AArch64 guests on QEMU `virt`: local, because no CI runner boots one yet.
    ("virt_early_panic", Sched::Parallel, Tier::Local),
    ("virt_early_fault", Sched::Parallel, Tier::Local),
    ("virt_el2_drop", Sched::Parallel, Tier::Local),
    ("virt_user_mode", Sched::Parallel, Tier::Local),
    ("virt_timer_preempts", Sched::Parallel, Tier::Local),
    ("virt_irq_storm", Sched::Parallel, Tier::Local),
    ("virt_timer_floor", Sched::Parallel, Tier::Local),
    ("virt_fp_isolation", Sched::Parallel, Tier::Local),
    ("virt_first_entry", Sched::Parallel, Tier::Local),
    ("virt_unmap_touch", Sched::Parallel, Tier::Local),
    ("virt_debug_refused", Sched::Parallel, Tier::Local),
    ("virt_readonly_copyout", Sched::Parallel, Tier::Local),
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

/// What `SYS_DEBUG` action 8 paints. Green, because the decoder thresholds on
/// the brightest channel and a colour a glyph could contain would let a
/// surviving pixel read as text rather than as itself.
const GRAFFITI: [u8; 3] = [0x00, 0xC0, 0x00];

/// Tests whose machine shape *is* the test: metal-sim, where the PS/2
/// keyboard is the only input source and no virtio device exists, or a q35
/// with the i8042 switched off. None of them can share the multi-test boot,
/// so each costs its own. `run_machine_test` dispatches them.
/// Feature-carrying ones last, as SCREEN_TESTS does: each distinct kernel
/// feature set is another kernel rebuild.
///
/// A few adjacent runs of names share *one* boot between them — see
/// [`group_boot`], which is what makes adjacency here load-bearing rather than
/// tidy.
const MACHINE_TESTS: &[(&str, Sched, Tier)] = &[
    ("ioapic_topology", Sched::Parallel, Tier::Fast),
    ("guest_dies_with_its_harness", Sched::Parallel, Tier::Fast),
    // The interrupt census adds up, and every device interrupt is still cpu0's.
    // **The second half is what makes this the track's instrument rather than a
    // tidiness check**: it states the present-state fact
    // `issues/kernel/every-interrupt-lands-on-the-boot-cpu.md` opens with, so
    // the day a placement policy lands this test is the first thing that reds
    // and the first number against a number. Parallel: every verdict is
    // arithmetic over counters the guest printed, and there is no clock in any
    // of it.
    ("irq_census_conservation", Sched::Parallel, Tier::Weekly),
    ("control_regs", Sched::Parallel, Tier::Fast),
    ("control_regs_negative", Sched::Parallel, Tier::Fast),
    // The boot facts the metal suite reads off a machine's own records: every
    // CPU the firmware named came up and none of their timestamp counters
    // trails the BSP's; the physical memory manager's accounting against the
    // firmware map balances to the byte; every ACPI table this kernel goes on
    // to decode checksummed; the LAPIC timer and the TSC calibrated to a
    // frequency; the PCI inventory's function count matches its rows; and one
    // machine-wide TLB shootdown's cost is a distribution rather than one
    // boot's average. Every verdict is arithmetic over records, with no clock
    // in the judging, so all six are Parallel.
    ("smp_roster_and_tsc_trail", Sched::Parallel, Tier::Nightly),
    ("pmm_accounting", Sched::Parallel, Tier::Nightly),
    // ROOT is the loader's image in memory: the kernel says it mounted it
    // from memory, and that init was spawned with no storage command issued;
    // and a loader that hands no image is a boot refused by name, never one
    // that goes to a disk for ROOT. Both are records of one boot each, with no
    // clock in the verdict.
    ("root_from_memory", Sched::Parallel, Tier::Nightly),
    ("root_withheld_refused", Sched::Parallel, Tier::Nightly),
    // A loader built to another `KernelArgs` layout is refused by name before
    // the kernel reads a field the layout could have moved.
    ("kernel_args_layout_refused", Sched::Parallel, Tier::Nightly),
    // The boot from power-on, as the kernel converts the loader's TSC readings:
    // judged against the loader's raw counts and the kernel's own rate.
    ("boot_from_power_on", Sched::Parallel, Tier::Nightly),
    ("acpi_table_inventory", Sched::Parallel, Tier::Nightly),
    ("timer_calibration", Sched::Parallel, Tier::Nightly),
    ("pci_inventory", Sched::Parallel, Tier::Nightly),
    ("smp_failed_ap_leaves_no_hole", Sched::Parallel, Tier::Weekly),
    ("input_merge", Sched::Parallel, Tier::Weekly),
    ("metal_sim_input", Sched::Parallel, Tier::Weekly),
    ("input_claim_absent", Sched::Parallel, Tier::Weekly),
    // One boot; every verdict is a PPM header field or a console line, and no
    // clock is in any of them.
    ("gpu_set_resolution", Sched::Parallel, Tier::Fast),
    // One boot from here to `metal_sim_compositor_stall` (`METAL_SIM_DESKTOP`).
    ("metal_sim_compositor", Sched::Parallel, Tier::Nightly),
    // Reads the boot log this group already has, after the member above has
    // drained it. Text only, no clock in the verdict.
    ("metal_sim_scanout_wc", Sched::Parallel, Tier::Nightly),
    ("metal_sim_window_caps", Sched::Parallel, Tier::Nightly),
    ("metal_sim_ipc_hostile_peer", Sched::Parallel, Tier::Nightly),
    ("metal_sim_compositor_stall", Sched::Parallel, Tier::Nightly),
    // Last of the group: it drops clients on purpose and its verdict is that
    // the desktop outlived every one of them.
    ("metal_sim_client_death", Sched::Parallel, Tier::Nightly),
    // A thousand pointer packets paced from the host, and not one assertion on
    // when any of them arrived: the settles are 400 ms against a driver that
    // acts in microseconds, both liveness loops run to 20 s, and the three
    // verdicts are a count of bound sources, a frame batch above the taskbar's
    // two, and a desktop still painting afterwards.
    ("metal_sim_pointer_churn", Sched::Parallel, Tier::Nightly),
    // A window dragged by injected pointer packets, and the exact opposite of
    // the churn above on the one question that decides this: here each packet's
    // effect has to be on screen before the next is sent. The press that starts
    // the drag must land on a title bar the previous motion put under the
    // cursor, and the drag's displacement is read back as a coordinate — so a
    // guest one batch behind aims at the content instead, which is a different
    // verdict rather than a slower one. Watched to happen, on a compositor made
    // slow on purpose. Its own boot too: it leaves the pointer somewhere else
    // and the window in a different place than it found them.
    ("metal_sim_window_drag", Sched::Serial, Tier::Weekly),
    // Its own boot: the compositor it abuses has to be one nothing else has
    // touched.
    ("metal_sim_hostile_clipboard", Sched::Parallel, Tier::Fast),
    // One C program through the toolchain's clang, read by the loader's decoder,
    // and one boot to run it.
    ("c_hello", Sched::Parallel, Tier::Fast),
    // One boot and one number, with no clock in the verdict: the frames are
    // counted in game tics, whatever the host's speed. Fast, because it is the
    // gate on what the C compiler makes of doom.
    ("doom_frames", Sched::Parallel, Tier::Fast),
    // ureq and rustls from crates.io, fetching over TLS 1.3 from a host this
    // test mints a CA for. Every verdict is a printed line or a digest; the
    // only clock is `run_test`'s ceiling.
    ("https_tls13", Sched::Parallel, Tier::Weekly),
    // The same judge over netd's Intel driver instead of the virtio one: the
    // 82574L QEMU models has the register file the T14's I219 has, so this is
    // where that driver moves real frames before the laptop does.
    ("https_tls13_e1000e", Sched::Parallel, Tier::Fast),
    // The log `logd` serves: a reader that connects after a job has ended is
    // handed the boot from its first line, and every line it is handed is the
    // line the guest's own `/log` holds, in its order. The verdicts are lines'
    // arrival and a line-for-line comparison; the clocks in it are liveness
    // guards on a guest that stopped talking.
    ("log_stream", Sched::Parallel, Tier::Nightly),
    // The same log over netd's Intel driver, for the same reason
    // `https_tls13_e1000e` exists: the T14's NIC is an I219 and this is the
    // only machine in reach that runs that driver.
    ("log_stream_e1000e", Sched::Parallel, Tier::Nightly),
    // A reader that never reads, beside a flood: the file and a second reader
    // are whole regardless.
    ("log_stream_stalled_reader", Sched::Parallel, Tier::Nightly),
    // A program's line in `/log`, on the served log and on the console, under
    // the name of the pipe it came out of. Lines and a comparison; no clock.
    ("log_program_line", Sched::Parallel, Tier::Nightly),
    // A program writing the kernel's words and another program's head: every
    // judge of `/log` reads the truth. Lines, and the exit judge; no clock.
    ("log_program_forgery", Sched::Parallel, Tier::Nightly),
    // A stop the kernel refuses after logd flushed for it: a line said after
    // it is in `/log`. Lines; no clock.
    ("log_after_a_refused_stop", Sched::Parallel, Tier::Nightly),
    // The same stop with logd's flush held until init resumes it: logd runs
    // the flush, then the resume, and lives. Lines; init's flush bound.
    ("log_resume_meets_its_flush", Sched::Parallel, Tier::Nightly),
    // A child flooding its parent's log ring while logd reads none of it: the
    // parent's next line is in `/log`. Lines; its clock is a guard.
    ("log_ring_keeps_the_owners_slots", Sched::Parallel, Tier::Nightly),
    // A program's line said after three batches of records, read before them:
    // `/log` carries it after every one. Lines and positions; no clock.
    ("log_program_line_after_its_records", Sched::Parallel, Tier::Nightly),
    // A program printing init's word accepting a swap of netd: logd turns
    // nobody away, and a reader after it is admitted. Lines; no clock.
    ("log_carrier_forgery", Sched::Parallel, Tier::Nightly),
    // A flood many times its ring: every line in `/log` in order or counted.
    ("log_program_flood", Sched::Parallel, Tier::Nightly),
    // netd taking this machine's address from the network instead of carrying
    // one written down. The DHCP server it is judged against is QEMU's own, an
    // implementation of RFC 2131 this repository did not write, and its lease
    // is known field by field. The verdicts are records and a lease's fields;
    // no clock in it.
    ("lan_dhcp_lease", Sched::Parallel, Tier::Nightly),
    // The lease probe on the same part: netd serves its window, exits with the
    // lease's verdict, and the report it leaves on the log volume names the
    // backend's lease with frames counted both ways and the lease kept across
    // a link flap. Records and a file; its clocks are the flap's hold and the
    // drain that outlasts netd's window, and a slower machine moves neither.
    ("lan_lease_report", Sched::Parallel, Tier::Weekly),
    // The T14's talking boot rehearsed on the same part: the log the guest
    // serves, read from its first line through a forward, one command over ssh
    // answered byte for byte, and `reboot` over ssh ending the guest. Lines,
    // bytes and a reset; its clocks are liveness guards on a guest that
    // stopped talking.
    ("lan_talk", Sched::Parallel, Tier::Nightly),
    // netd answering for its name: a resolver's query through a forward onto
    // the guest's multicast DNS port is answered with the lease's address, and
    // one for another name is not answered. Bytes; its clocks are a guard on
    // the answer and the window the unanswered query is given.
    ("lan_mdns_answer", Sched::Parallel, Tier::Nightly),
    // A running service's binary replaced with no reboot: netd swapped for its
    // rebuild over ssh while the stream runs, on virtio-net and on the 82574
    // the T14's I219 shares a register file with; a wrong digest and a
    // stranger's key refused with netd untouched; a replacement that panics at
    // once answered by the old binary running again. Records, bytes and the
    // guest's own `/log`; every clock is a liveness guard on a guest that
    // stopped talking, and probation is init's.
    ("swap_netd", Sched::Parallel, Tier::Nightly),
    // The machine updates itself: an image over `ssh … update` is written to
    // the idle slot and is the kernel the next boot runs; every slot the loader
    // must refuse is refused by name and the other boots; and a slot whose
    // kernel dies falls back on its own, its death in the next boot's `/log`.
    // The same machine's floor, grant and hang: a floor is its key's and its
    // image's, init grants nothing the slot table names but an idle slot's
    // partition, and a hang of an unproven image is a death.
    ("update_boots_the_new_kernel", Sched::Parallel, Tier::Weekly),
    ("update_refusals_boot_the_other_slot", Sched::Parallel, Tier::Weekly),
    ("update_falls_back_from_a_dying_kernel", Sched::Parallel, Tier::Weekly),
    ("update_hang_kills_an_unproven_image", Sched::Parallel, Tier::Nightly),
    ("update_grant_refuses_a_stray_partition", Sched::Parallel, Tier::Nightly),
    ("update_floor_is_the_images_own", Sched::Parallel, Tier::Weekly),
    ("update_refused_pass_credits_no_image", Sched::Parallel, Tier::Nightly),
    ("lan_swap", Sched::Parallel, Tier::Nightly),
    ("swap_refusals", Sched::Parallel, Tier::Nightly),
    ("swap_crash_rolls_back", Sched::Parallel, Tier::Nightly),
    // The 82574 swapped to a holder that stops it and masters it while the
    // host sends it frames. The verdict is the kernel's console; its clocks
    // are liveness guards.
    ("swap_quiets_the_function", Sched::Parallel, Tier::Nightly),
    // The same part released as though nothing could reset it, the way the
    // T14's I219 is, and swapped to a holder that masters it with its receive
    // unit still on. The verdict is the kernel's console; its clocks are
    // liveness guards.
    ("swap_keeps_what_nothing_reset", Sched::Parallel, Tier::Nightly),
    // The same part swapped to a holder that aims it outside its grant and
    // waits on its claim. The verdict is the holder's own word on what the
    // faulted claim answered; its clocks are liveness guards.
    ("swap_fault_tells_its_holder", Sched::Parallel, Tier::Nightly),
    // An `igb` netd held, released by an Express function level reset and
    // claimed again by netd's replacement, which reads it through the window
    // its claim maps. The verdict is what the function answers there.
    ("swap_resets_the_function", Sched::Parallel, Tier::Nightly),
    // The same `igb`, its window left where its reset put it: the replacement
    // is refused it, and the verdict is init failing the swap by the
    // device's name rather than putting netd in service without it.
    ("swap_refused_device_fails", Sched::Parallel, Tier::Fast),
    // The same, its window moved inside the cut rather than lost: the kernel
    // reads the address the register holds, not only that it holds one.
    ("swap_moved_device_fails", Sched::Parallel, Tier::Nightly),
    // A program sshd runs undeclared asks for the swap port. The verdict is
    // the program's own exit: the port is not in the namespace it inherited.
    ("swap_not_inherited", Sched::Parallel, Tier::Nightly),
    // The same client on a wire with no server: it says it has no address and
    // announces itself anyway. Its verdict waits out netd's own lease bound, so
    // a slower machine moves it.
    ("lan_no_lease", Sched::Parallel, Tier::Weekly),
    ("netd_connection_caps", Sched::Parallel, Tier::Nightly),
    // `inspect` against the four owners on the one boot that runs them all,
    // with the negative control's binary run on it too. Every verdict is the
    // set of lines a selector printed; no clock in any.
    ("inspect_reads_its_owners", Sched::Parallel, Tier::Nightly),
    // The netcase boot again: netd must not abort a listener on a ring flag its
    // own client forged. Its verdict is a kernel-reported EOF or its absence;
    // no clock in it.
    ("netd_listener_forgery", Sched::Parallel, Tier::Weekly),
    // The netcase boot again: a receiver that stops reading until its pipe
    // is full still gets every byte of a stream past it. The verdict is the
    // guest's byte-for-byte comparison; its clocks are liveness guards.
    ("netd_slow_reader", Sched::Parallel, Tier::Nightly),
    // The netcase boot again: client pipes netd cannot use, or loses under
    // it, cost that client its connection and never netd. The verdict is a
    // round trip after each case, a named line per refusal and a clean
    // console; its clocks are liveness guards.
    ("netd_refused_pipes", Sched::Parallel, Tier::Nightly),
    // The netcase boot again: an accept netd refuses for room leaves its owner
    // a wake for the connection it left, once room returns. The verdict
    // is the guest's wake or its absence.
    ("netd_refused_accept", Sched::Parallel, Tier::Fast),
    // The netcase boot again: bytes held back past a full pipe move on the
    // pipe's room alone, the peer holding the connection open and silent. The
    // verdict is the guest's byte-for-byte comparison; its clocks are
    // liveness guards.
    ("netd_held_open", Sched::Parallel, Tier::Nightly),
    // The netcase boot again, beside a host UDP echo: a datagram the client's
    // pipe will not take whole ends that socket by name and no other. The
    // verdict is each socket's answer; its clocks are liveness guards.
    ("netd_udp_refused", Sched::Parallel, Tier::Nightly),
    // The netcase boot again, beside a host UDP echo: a socket bound through
    // std to 0.0.0.0 receives the echo's unicast reply. The verdict is the
    // reply's bytes; its clock is a liveness guard.
    ("netd_udp_any_address", Sched::Parallel, Tier::Nightly),
    // The netcase boot, whose user network forwards 10.0.2.3 to this host's
    // resolver: `host` resolves a real name to the addresses this host's
    // resolver gives it, and a `.invalid` name to none. The verdict is the
    // two answers; its clocks are liveness guards.
    ("dns_resolve", Sched::Parallel, Tier::Nightly),
    // The netcase boot, every frame it sends held once it has its lease:
    // lookups whose clients hung up or spoke again are let go at once, and
    // one nobody answers ends when its schedule does. The verdict is netd's
    // answers.
    ("netd_lookup_let_go", Sched::Parallel, Tier::Nightly),
    // Two netcase boots, each frame put on the wire kept: the first DHCP
    // transaction ID of each differs, because netd seeds smoltcp's random
    // source from the kernel's. The verdict is two numbers off the wire.
    ("netd_seeds_its_stack", Sched::Parallel, Tier::Nightly),
    // The netcase boot with two programs naming one PCI function: the verdict
    // is which of them the kernel let have it. Console lines only, no clock.
    ("pci_function_is_exclusive", Sched::Parallel, Tier::Nightly),
    // The same boot again, read for what the kernel asked the machine before it
    // moved that function's BAR. Its own boot rather than a second assertion in
    // the row above, because that one's subject is exclusivity and a test that
    // reds tells a reader which of the two it is about. It waits out a drain for
    // the message record, so its price carries a fixed span of host wall clock.
    ("bar_placement_is_proven", Sched::Parallel, Tier::Nightly),
    // Its own boot with a blank DATA volume, asked over ssh where everything
    // it wrote went. Every verdict is a listing or a line; no clock in any.
    ("layout_fresh_boot", Sched::Parallel, Tier::Nightly),
    // One `SSHD_LOGIN` boot for the three, driven by `tests/ssh-client-host`.
    // Adjacent because `group_of` makes adjacency load-bearing, and one tier
    // because one boot cannot be in two. Every verdict is bytes or an exit
    // status; the client's own ceiling is a liveness guard and no assertion
    // reads a clock.
    ("sshd_exec", Sched::Parallel, Tier::Nightly),
    ("sshd_files", Sched::Parallel, Tier::Nightly),
    ("sshd_key_auth", Sched::Parallel, Tier::Nightly),
    ("netd_hostile_peer", Sched::Parallel, Tier::Weekly),
    ("launcher_refusals", Sched::Parallel, Tier::Weekly),
    // Paths a child prints and the kernel's refusals by name; no clock in any of them.
    ("spawn_cwd", Sched::Parallel, Tier::Nightly),
    ("foreign_disk_untouched", Sched::Parallel, Tier::Weekly),
    ("volume_from_another_disk", Sched::Parallel, Tier::Weekly),
    ("broken_data_volume_is_absent", Sched::Parallel, Tier::Nightly),
    ("data_candidate_with_bad_geometry_is_absent", Sched::Parallel, Tier::Nightly),
    // Four kernel lines and a file read off the image once the guest is gone; no clock in any of them.
    ("internal_disk_boot", Sched::Parallel, Tier::Weekly),
    // One boot each, kernel lines and image bytes for verdicts, no clock in either.
    ("block_duplicate_id", Sched::Parallel, Tier::Weekly),
    ("page_cache_partition_offset", Sched::Parallel, Tier::Weekly),
    // A partition claimed as a device: one boot, every refusal in the guest,
    // the neighbours and the target judged off the image. Body in
    // `tests/common/partclaim.rs`, as are the two below.
    ("partition_claim", Sched::Parallel, Tier::Fast),
    // Three boots: a disk that does not answer a read of its table, every
    // attempt refused until the deadman, and ROOT's source withheld from every
    // claim once its disk did not answer the boot's hold.
    ("partition_claim_gives_up", Sched::Parallel, Tier::Nightly),
    // Three boots, a USB stick's device leaving owing one claim's write and
    // coming back on another port each time: each partition's fsync answers
    // for its own writes, across a close, after another's flush, and at the
    // shutdown when nobody asked.
    ("partition_claim_departure", Sched::Parallel, Tier::Nightly),
    // A same-length overwrite on /home, the guest's read held against the image. Body in `tests/common/storage.rs`.
    ("home_overwrite_reads_back", Sched::Parallel, Tier::Weekly),
    // One filesystem under two paths: the guest writes under each of /apps and
    // /home, the host finds both in one volume on the image. Body in
    // `tests/common/storage.rs`.
    ("apps_and_home_are_one_filesystem", Sched::Parallel, Tier::Weekly),
    // `pkg install <file>` from a local archive and gbae's first run: the whole
    // package path in one boot, judged off the DATA volume once the guest is
    // gone. Body in `tests/common/pkg.rs`.
    ("pkg_install_gbae", Sched::Parallel, Tier::Nightly),
    ("boot_partition_identity", Sched::Parallel, Tier::Nightly),
    // One boot of its own, because it ends the machine. Every verdict is a
    // kernel line or the stop reason QEMU reported; no clock is in either.
    ("machine_reboot", Sched::Parallel, Tier::Weekly),
    // Its own boot: every verdict is a console line, QEMU's stop reason or a record off the image.
    ("metal_job_reboot", Sched::Parallel, Tier::Fast),
    // Its own boot, and the one whose numbers are the T14's: what it judges
    // here is the plumbing, since every span on an emulated device is a fact
    // about TCG.
    ("metal_device_probe", Sched::Parallel, Tier::Nightly),
    // Its verdict waits out a staged window.
    ("job_deadline_reboots", Sched::Parallel, Tier::Nightly),
    // Its own boot, and its verdict waits out the same staged window.
    ("quiesce_stops_the_machine", Sched::Parallel, Tier::Fast),
    // Its own boot: it ends the machine, and its verdict is the order of
    // kernel lines.
    ("quiesce_refuses_a_second_shutdown", Sched::Parallel, Tier::Nightly),
    // Its own boot: it ends the machine, and its verdict is the volume that
    // boot leaves.
    ("quiesce_leaves_the_volume_whole", Sched::Parallel, Tier::Nightly),
    // Its own boot each: it ends the machine, and its verdict is the stop
    // record that boot writes.
    ("quiesce_wakes_on_the_last_park", Sched::Parallel, Tier::Nightly),
    ("quiesce_wakes_on_the_last_teardown", Sched::Parallel, Tier::Nightly),
    // Two reads of `TCO_RLD` straddling a real-time stall, so a slower machine
    // changes the verdict.
    ("loader_watchdog_arms", Sched::Parallel, Tier::Nightly),
    // Its own boot, and the verdict is QEMU's stop reason inside the bound.
    ("watchdog_resets", Sched::Parallel, Tier::Nightly),
    // The panicked kernel's own bound, which is what ends a boot on a machine
    // whose chipset timer does not count. The verdict is QEMU's stop reason and
    // the line saying the bound ran out.
    ("panic_reboots", Sched::Parallel, Tier::Nightly),
    // The same verdict from inside `percpu::init_bsp`: the earliest point a
    // panic is reportable, and the window the owner's T14 stops in.
    ("panic_before_peripherals_reboots", Sched::Parallel, Tier::Nightly),
    // The boot chain's three answers. The two chain names each watch a guest
    // take its own reset and read the pass after it, so both are anchored to
    // the bound the first boot counts down.
    ("blackbox_panic_chain", Sched::Parallel, Tier::Nightly),
    ("blackbox_done_chain", Sched::Parallel, Tier::Nightly),
    // The one bound in this tree that ends a machine nothing else can: a boot
    // whose every CPU has stopped taking scheduler passes. Its verdict is a
    // bound counted down in the guest.
    ("boot_deadline_ends_a_wedge", Sched::Parallel, Tier::Nightly),
    // The same bound ending the same machine with a device in its hands.
    ("usb_reset_records_the_phase_it_cut", Sched::Parallel, Tier::Weekly),
    // The other half of that same parameter, and the state its poll cannot
    // reach: one CPU with interrupts off, which no running CPU can see. Two
    // bounds counted down in the guest, so it belongs beside the row above.
    ("hard_lockup_ends_a_deaf_cpu", Sched::Parallel, Tier::Nightly),
    // The control on both of those bounds standing down: a panic whose panel is
    // still up when the deadline expires must cross the reset as a panic report
    // and never as a `WEDGED` page.
    ("panic_outlives_the_deadline", Sched::Parallel, Tier::Nightly),
    // Four chained boots, one per way this kernel reaches a reset, each
    // anchored to the bound its own first boot counts down.
    ("usb_reset_hands_devices_back", Sched::Parallel, Tier::Weekly),
    // The control on the chain: a record another image left in the same memory
    // is cleared and its pass boots a kernel, where a real predecessor's ends
    // the chain. One boot, one actuator.
    ("blackbox_foreign_record", Sched::Parallel, Tier::Fast),
    // Three launches of one image file, and the third is the one that makes it
    // a bound: a hang costs the machine one boot and never traps it.
    ("hang_bounded_by_the_stick", Sched::Parallel, Tier::Nightly),
    // Its own boot, and every verdict is a line: no host clock in any of it.
    ("blackbox_unclaimed_page", Sched::Parallel, Tier::Nightly),
    // The seal read off the page's own bytes by QEMU, after a panic earlier than
    // anything the kernel used to learn the page's address from. Its own boot,
    // and no clock in the verdict.
    ("blackbox_early_panic_sealed", Sched::Parallel, Tier::Nightly),
    // The same crash on the owner's machine's own shape — no serial port at all
    // — where the panel and the page are the only two channels there are.
    ("blackbox_early_panic_sealed_muted", Sched::Parallel, Tier::Nightly),
    // The exception entry's own seal, off the page's bytes on an ordinary boot.
    ("blackbox_fault_sealed", Sched::Parallel, Tier::Nightly),
    ("double_fault_stack", Sched::Parallel, Tier::Nightly),
    // One boot of its own, ten seconds of Ring 3 spinning, and every verdict is
    // a count the kernel printed or a line it printed: how many NMIs landed at
    // CPL 0 with a user `rsp`, against how many landed in Ring 3, both off the
    // same storm. No host clock is in any of it — the ten seconds are how long
    // the victim spins, not a margin anything is measured against — so Parallel.
    ("syscall_window_nmi", Sched::Parallel, Tier::Weekly),
    // The two controls on the name above: the kernel with vector 2's IST index
    // taken off, which must double fault at the entry on the NMI aimed at the
    // CPU the storm holds inside it, with `cr2 = rsp - 8` at the held `rsp`, and
    // the one nested NMI an early `iretq` can stage, which must take the loud
    // path. Both boots end in a halted machine that has to be drained past its
    // own report, which is where the price is. Nothing in either verdict is a
    // duration.
    ("syscall_window_nmi_controls", Sched::Parallel, Tier::Weekly),
    // Its own boot, its own feature, and it drives the guest only through
    // stdin — nothing it touches is shared with another test.
    ("idle_stack_guard", Sched::Parallel, Tier::Weekly),
    // The same dump asked for inside the passes that may not serve it, on one
    // CPU. Parallel: every verdict is a line the guest prints or a count the guest
    // keeps, and no duration is in any of them.
    ("dump_left_pending_is_owed", Sched::Parallel, Tier::Nightly),
    ("diskless_boot", Sched::Parallel, Tier::Nightly),
    // Every verdict is a line of text or a device property, and no clock is in
    // any of them.
    ("virtio_net_no_msix", Sched::Parallel, Tier::Fast),
    // One boot of the NIC config with a staged capability list, and every
    // verdict a console line. No clock in any of them.
    ("pci_claim_caps_truncated", Sched::Parallel, Tier::Nightly),
    // One boot, and its verdict is a line the kernel printed before any device
    // was brought up. No clock and no device in it.
    ("virtio_used_ring", Sched::Parallel, Tier::Weekly),
    // A fatal path with other CPUs running userland that makes kernel records:
    // none of theirs follows the fatal path's own line after the stop beyond
    // the one each may have had in flight. Order, and no clock in it.
    ("panic_halts_the_others_first", Sched::Parallel, Tier::Nightly),
    // A kernel log line from PCI enumeration; no clock and no real device in it.
    ("pci_capability_walk", Sched::Parallel, Tier::Weekly),
    // What QEMU was told to create against what the guest enumerated: two
    // accounts of one bus from two independent readers. One boot, every
    // verdict a set comparison.
    ("query_pci_agreement", Sched::Parallel, Tier::Weekly),
    // The root `system.toml`, booted rather than read: the shipped init list
    // and program namespaces have no other gate a harness run can reach.
    // Every verdict is a console line.
    ("shipped_config_boots", Sched::Parallel, Tier::Fast),
    // One boot whose verdict is three lines of kernel log and a census column.
    // The two waits inside the guest are bounded and report rather than hang, so
    // no host clock decides anything.
    ("lapic_spurious_vector", Sched::Parallel, Tier::Weekly),
    // One boot with both stuck-device actuators armed.
    ("driver_wait_refused", Sched::Parallel, Tier::Weekly),
    // One boot; the leak-rollback controls' two verdict lines.
    ("leak_rollback_selftest", Sched::Parallel, Tier::Weekly),
    // One boot; the reopen control's one verdict line.
    ("process_reopen_selftest", Sched::Parallel, Tier::Weekly),
    // One boot; three read-fault control verdicts.
    ("read_fault_selftests", Sched::Parallel, Tier::Weekly),
    ("xhci_many_devices", Sched::Parallel, Tier::Weekly),
    // Its whole assertion is that a keystroke injected from the host crossed a
    // USB keyboard on the *second* controller, and `input_events_run` sends
    // each one only after the guest has printed the last — so a key the host
    // never got to send is a stall it names, and never a key the driver lost.
    ("xhci_second_controller", Sched::Parallel, Tier::Weekly),
    ("xhci_two_controllers", Sched::Parallel, Tier::Weekly),
    // **Returned 2026-08-17**, on the same `input_events_run` the two names
    // above it run: it had `xhci_second_controller`'s sequence written out again
    // on fixed sleeps, and nothing sent the right-button release
    // `test_rs_input_events` exits on — so 30 s of its 35.2 s CI price was a
    // client waiting out a fallback deadline with every assertion already
    // satisfied.
    ("xhci_msi_only", Sched::Parallel, Tier::Weekly),
    ("xhci_no_interrupt", Sched::Parallel, Tier::Weekly),
    ("nvme_large_device", Sched::Parallel, Tier::Nightly),
    ("nvme_wide_sector", Sched::Parallel, Tier::Weekly),
    ("iommu_discovery", Sched::Parallel, Tier::Weekly),
    ("readdir_bound", Sched::Parallel, Tier::Weekly),
    // Its own boot: it fills the VFS `created_dirs` cap and leaves it there.
    ("mkdir_cap", Sched::Parallel, Tier::Weekly),
    // Two boots, and the verdict is that they answer differently. Nothing in it
    // is timed: every arm is a process exit code or a byte comparison.
    ("fpu_isolation", Sched::Parallel, Tier::Nightly),
    // Two boots, exit codes only (no clock, Parallel).
    ("gsbase_locked", Sched::Parallel, Tier::Weekly),
    // The fourth declared kernel build, booted so that the scheduler core's
    // `feature = "check"` instruments are compiled and executed by a CI run at
    // all.
    ("sched_check_build", Sched::Parallel, Tier::Fast),
    // What nesting a `scheduler::Operation` may and may not do, which is a law
    // with no host-side reader: the type reaches `percpu::cpu_id` and
    // `driver::current_handle`, so nothing outside a booted machine can
    // construct one. Parallel, and nothing in it is a duration — every verdict
    // is a comparison between two numbers the kernel printed, both of them
    // offsets it chose itself.
    ("operation_nesting", Sched::Parallel, Tier::Weekly),
    ("short_sleep_livelock", Sched::Parallel, Tier::Fast),
    // The spawn half alone: one headless boot whose verdict is kernel log
    // lines.
    ("klogd_hosted", Sched::Parallel, Tier::Weekly),
    ("klogd_fault_halts", Sched::Parallel, Tier::Nightly),
    ("syscall_panic_halts", Sched::Parallel, Tier::Nightly),
    ("syscall_fault_halts", Sched::Parallel, Tier::Nightly),
    ("lock_across_switch_halts", Sched::Parallel, Tier::Nightly),
    ("heap_over_ceiling_halts", Sched::Parallel, Tier::Nightly),
    // The two dead ends of the panic path, each staged on purpose and read for
    // what the machine manages to say on its way out. Each boot dies inside the
    // boot phases at the marker the harness waits for, so neither pays for a
    // userland. Parallel:
    // every verdict is a substring of a report the guest wrote, and there is no
    // clock in any of it.
    ("reentry_names_the_first_panic", Sched::Parallel, Tier::Weekly),
    // The kernel hasher's boot-order obligation, in the row above's shape and
    // for its reasons.
    ("hash_seed_precedes_every_map", Sched::Parallel, Tier::Weekly),
    ("double_panic_names_the_fault", Sched::Parallel, Tier::Weekly),
    // The third shape: a `#PF` inside a panic, which is the one
    // `fatal_exception`'s recursive short-circuit exists for and the one it
    // never classified. Same boot shape as its two neighbours — dies inside the
    // boot phases at the marker, no userland — so Parallel.
    ("nested_fault_is_recursive", Sched::Parallel, Tier::Weekly),
    // The conservation law across `SYS_LOG_READ`, and the nesting gate at one
    // CPU. Parallel: every verdict is a ledger the
    // guest computes over its own records — every sequence number read or
    // counted lost, every payload regenerated byte for byte — and not one of
    // them reads a clock. A loaded host makes the producers outrun the reader
    // further, which moves records from `read` into `lost` and leaves the law
    // exactly where it was.
    ("log_conservation_smp2", Sched::Parallel, Tier::Weekly),
    ("log_nested_emit", Sched::Parallel, Tier::Weekly),
    // The same interrupt one window earlier — between a record's shard-pointer
    // read and its `xadd` — and its negative control, which is the only reader
    // `log-unbracketed-reserve` has ever had. Parallel for
    // `log_nested_emit`'s reasons: both verdicts are the guest's ledger over its
    // own records, one saying the shard kept a single order and the other that
    // it lost it by name, and no clock is in either.
    ("log_reserve_window", Sched::Parallel, Tier::Weekly),
    ("log_reserve_window_negative", Sched::Parallel, Tier::Weekly),
    // A guest writes a daemon-shaped line into a real capture window on purpose
    // and the real comparison ignores it, with the filter turned off as the
    // control. One boot, two `echo`s, and every verdict is a string comparison
    // the host makes over a capture — no clock in it.
    ("c_capture_ignores_daemon_lines", Sched::Parallel, Tier::Weekly),
    // A poll on the machine's log against a *handle* going away. Parallel: both
    // halves are verdicts the guest computes — a completion count
    // immediately after a close, retried against a record arriving in the same
    // microseconds, and a completion afterwards bounded far above the two
    // scheduler passes it needs.
    ("log_poll_outlives_a_close", Sched::Parallel, Tier::Weekly),
    // The same question asked of the keyboard, where two *kinds* of object name
    // one source: a poll on stdin against the keyboard claim going away, a poll
    // on the mouse claim against its own, and an injected keystroke to show the
    // first was still armed. Parallel: two of the three verdicts are
    // counts the guest takes immediately after a close on its own thread, and
    // the third is bounded far above the one interrupt it waits for.
    ("keyboard_claim_close_spares_stdin", Sched::Parallel, Tier::Fast),
    // One boot that stops dead in phase 3, read for what it managed to say.
    ("pre_idle_wedge_speaks", Sched::Parallel, Tier::Weekly),
    ("i8042_health", Sched::Parallel, Tier::Nightly),
    // And one from here to `i8042_mouse` (`I8042_TRACE`). Neither measures a
    // rate: nothing goes out until the guest has reported what the injection
    // before it produced — `i8042_mouse` within [`MOUSE_LEAD`], the keyboard one
    // a group at a time — so a guest with less of the host is a longer run and
    // not a smaller count.
    ("i8042_no_spurious_wake", Sched::Parallel, Tier::Nightly),
    ("i8042_mouse", Sched::Parallel, Tier::Nightly),
    // A boot each, and deliberately not a group: every one of them changes
    // the machine's layout, and a wizard that exits the instant it has its
    // answer leaves the guest with nothing to run — so a later member reads a console the previous one is
    // still draining into.
    //
    // Each is a wizard conversation typed from the host, and that used to make
    // them serial on the grounds that a dropped keystroke reads like the defect
    // they exist to catch. What actually drops a keystroke is the *device*
    // queue, not the host's clock: QEMU's PS/2 controller holds sixteen bytes
    // and none of these conversations puts more than a handful in flight before
    // waiting on what the guest printed back. Every wait here is `serial_until`
    // against a marker with a twenty-second ceiling, so a slower guest is a
    // slower test and not a different verdict — which is the same argument
    // `i8042_kbd_echo` has run on at width 4 since the phase landed.
    ("swiss_german_layout", Sched::Parallel, Tier::Nightly),
    // One `LOCALE_WIZARD` boot for the pair since the drainer was made
    // runnable at commit — the boot-apiece and the injected drain keys it took
    // to share one were both the closed log-ring lag. Adjacent because
    // `group_of` makes adjacency load-bearing.
    ("locale_detect", Sched::Parallel, Tier::Nightly),
    ("locale_detect_unrecognized", Sched::Parallel, Tier::Nightly),
    // The wizard on the two surfaces the machine actually has, rather than on
    // the stand-in `locale_gate` is. Each costs a boot of a different image.
    ("console_locale_detect", Sched::Parallel, Tier::Fast),
    ("desktop_locale_detect", Sched::Parallel, Tier::Fast),
    // Typing at the same desktop, measured rather than transcribed: it waits
    // for its eight echoes instead of asserting how many arrived in a window,
    // so a guest that is slow costs seconds and not a verdict, and the verdict
    // itself is a fraction of the screen that no amount of load moves.
    ("desktop_typing_damage", Sched::Parallel, Tier::Nightly),
    ("desktop_window_child", Sched::Parallel, Tier::Nightly),
    // An unmodified iced app on the desktop, launched from the shell: the
    // window, its text in the system font, no redraw it did not ask for, and
    // a clean exit when the compositor closes it.
    ("toolkit_iced", Sched::Parallel, Tier::Weekly),
    // The wait every winit loop blocks in, the loop itself through winit's
    // API, and an animation held to the compositor's frame events.
    ("toolkit_window_wake", Sched::Parallel, Tier::Nightly),
    ("toolkit_winit_loop", Sched::Parallel, Tier::Nightly),
    ("toolkit_winit_pace", Sched::Parallel, Tier::Nightly),
    // Ctrl+Alt+D on the same machine. Parallel: it waits for a marker and its
    // verdicts are counts the report has to agree with itself about, not a
    // wall-clock margin — the one duration in it is the dump's own 250 ms
    // ceiling, which the guest spends and the host never measures.
    ("blocked_dump", Sched::Parallel, Tier::Nightly),
    ("i8042_absent", Sched::Parallel, Tier::Nightly),
    // The fault quarantines (masks) the controller's GSI: the line and its
    // count are the verdict, and no program runs.
    ("i8042_quarantine", Sched::Parallel, Tier::Nightly),
    ("i8042_budget_expiry", Sched::Parallel, Tier::Nightly),
    ("i8042_fadt_denial", Sched::Parallel, Tier::Weekly),
    ("i8042_kbd_echo", Sched::Parallel, Tier::Nightly),
    ("i8042_undecoded_bytes", Sched::Parallel, Tier::Fast),
    ("xhci_xecp_walk", Sched::Parallel, Tier::Weekly),
    ("xhci_slot_exhaustion", Sched::Parallel, Tier::Weekly),
    ("usb_storage_gate", Sched::Parallel, Tier::Weekly),
    ("usb_storage_shapes", Sched::Parallel, Tier::Weekly),
    ("usb_refused_disk_first", Sched::Parallel, Tier::Weekly),
    ("xhci_scan_hands_over_a_free_slot", Sched::Parallel, Tier::Nightly),
    // The owner's freeze, staged: `device_del` on the stick carrying `/boot`
    // and `/log` while the desktop draws. Serial because both verdicts are
    // liveness ceilings — two 2 s compositor reporting intervals inside 20 s,
    // and a console round trip inside 20 s — and a guest sharing the host with
    // eleven others answers those late for reasons that are not the defect.
    ("usb_boot_stick_pulled", Sched::Serial, Tier::Nightly),
    ("usb_pool_exhausted", Sched::Parallel, Tier::Weekly),
    ("usb_short_read", Sched::Parallel, Tier::Weekly),
    ("usb_storage_write_error", Sched::Parallel, Tier::Weekly),
    ("usb_flush_optional", Sched::Parallel, Tier::Nightly),
    ("xhci_deaf_registers", Sched::Parallel, Tier::Weekly),
    ("xhci_slow_connect", Sched::Parallel, Tier::Nightly),
    ("xhci_portsc_rw1c", Sched::Parallel, Tier::Weekly),
    // One staged break and no other, which puts the driver's recovery finishing
    // on its first try in the verdict: a retried command that reaches an
    // endpoint still halted from the staged break logs a second `transport
    // broke`, and how many tries it takes is how much of the host the guest
    // had — its own doc says one break under KVM and two under TCG off the
    // same tree, which is the race timer-anchored, not a margin, describes.
    ("usb_transport_break", Sched::Serial, Tier::Nightly),
    ("xhci_full_speed_device", Sched::Parallel, Tier::Weekly),
    ("xhci_superspeed_ports", Sched::Parallel, Tier::Weekly),
    // `xhci_flap` is the one that genuinely races the host against the guest:
    // its two QMP writes have to land inside *one* 100 ms debounce or the state
    // under test never happens, and it says so — `no replug collapsed inside a
    // debounce, so this run never staged the race`. A host that delays the
    // second write past 100 ms turns a green machine red with that sentence,
    // which is indistinguishable from the driver defect it hunts.
    ("xhci_flap", Sched::Serial, Tier::Nightly),
    ("xhci_descriptor_walk", Sched::Parallel, Tier::Weekly),
    ("esp_filesystem", Sched::Parallel, Tier::Nightly),
    // Three boots: a budget-refused flush retried and kept, the deadman's
    // declared death, and a hung device's failed reset escalation — the three
    // exits of `object/ops.rs`'s fsync loop. Every verdict is line presence
    // and host-side bytes, never a wall-clock margin.
    ("log_flush_retry", Sched::Parallel, Tier::Nightly),
    ("toybox_cp_volume", Sched::Parallel, Tier::Weekly),
    ("kernel_log_file", Sched::Parallel, Tier::Nightly),
    ("kernel_heartbeat", Sched::Parallel, Tier::Nightly),
    // The five RTC/firmware shapes, one kernel build and one boot each. Five
    // registrations because the artifact memo builds one kernel per feature
    // set anyway, so the split costs nothing and the parallel phase gets five
    // jobs it can place instead of one serial five-boot job it cannot.
    ("wall_clock_rtc_dead", Sched::Parallel, Tier::Weekly),
    ("wall_clock_rtc_unstable", Sched::Parallel, Tier::Weekly),
    ("wall_clock_no_century", Sched::Parallel, Tier::Weekly),
    ("wall_clock_century_register", Sched::Parallel, Tier::Weekly),
    ("wall_clock_utc", Sched::Parallel, Tier::Weekly),
    ("file_mtime_survives_a_reboot", Sched::Parallel, Tier::Nightly),
    ("file_mtime_undated", Sched::Parallel, Tier::Nightly),
    // `xhci_slow_connect`'s shape against the disk's port, but its actuator
    // masks the port until `BOOT_SCAN_DONE` — a kernel event, not a duration —
    // so what it stages is an ordering with no wall-clock margin on either
    // side: nothing here needs the serial tail.
    ("late_storage_connect", Sched::Parallel, Tier::Weekly),
    ("log_backing_read_error", Sched::Parallel, Tier::Weekly),
    ("boot_volume_metadata_error", Sched::Parallel, Tier::Weekly),
    ("log_partition_layout", Sched::Parallel, Tier::Weekly),
    // What the loader does with a slot's ROOT: bytes its signature does not
    // cover, a parameter naming another, an overlapping partition and an
    // unreadable chunk refused by name, and a twin on the boot disk or on
    // another disk never read. Serial, not by association: each stages a whole
    // boot image, one a second 32 GiB stick beside it.
    ("root_candidate_malformed", Sched::Serial, Tier::Weekly),
    ("root_named_but_absent", Sched::Serial, Tier::Weekly),
    ("root_chunk_refused", Sched::Serial, Tier::Nightly),
    ("root_chunk_refused_on_a_usb_stick", Sched::Serial, Tier::Fast),
    ("root_candidate_overlaps", Sched::Serial, Tier::Nightly),
    ("root_named_twice_on_the_boot_disk", Sched::Serial, Tier::Nightly),
    ("root_named_twice", Sched::Serial, Tier::Weekly),
    ("log_partition_identity", Sched::Parallel, Tier::Weekly),
    ("cache_eviction", Sched::Parallel, Tier::Nightly),
    // The write-back queue's three negative controls (wall 4 of
    // `issues/kernel/every-wait-in-this-kernel-is-a-spin.md`). `writeback_reopen`
    // and `writeback_spawn` arm `writeback-stall`, so each needs its own actuator
    // boot: one holds the queue open across a *handle* re-open, which the file
    // cache answers, and the other across a *spawn*, which is a device view and
    // does not. `writeback_durability` is a host-side volume oracle that shuts the
    // guest down and reads `/log` back with `toyos-fat32-check`.
    // The watch's lost-wake window, staged: `watch-window` holds every pipe
    // waiter between reading its condition and parking, so the peer's post lands where
    // only the notified bit carries it to the commit.
    ("blocking_read_window", Sched::Parallel, Tier::Nightly),
    // A sibling's munmap and mmap staged between a typed copy's translation
    // and its store (`copy-meets-a-remap`): the store never reaches the region
    // mapped after it.
    ("user_copy_races_munmap", Sched::Parallel, Tier::Nightly),
    // A sibling's store staged between a thread's TLS block being placed and
    // its rebase (`tls-rebase-window`): the block is never reachable there.
    ("tls_rebase_window", Sched::Parallel, Tier::Nightly),
    ("writeback_reopen", Sched::Parallel, Tier::Weekly),
    ("writeback_spawn", Sched::Parallel, Tier::Weekly),
    ("writeback_durability", Sched::Parallel, Tier::Fast),
    // `KernelHw::switch`'s SS reload (AMD `X86_BUG_SYSRET_SS_ATTRS`) observed the
    // one way a guest can, since its `SYSRET` does not reproduce the erratum. Reds
    // the day that `mov ss` leaves the switch.
    ("sysret_ss_reload", Sched::Parallel, Tier::Weekly),
    // The FAT32 read side's revocation gate, and a host-side volume oracle for
    // the same reason `writeback_durability` is one: whether the clusters the
    // unlink freed were really reissued, and whether the cycle left a volume, are
    // both questions the guest that staged them cannot answer about itself.
    ("fat_backing_revoked", Sched::Parallel, Tier::Weekly),
    // F5 and F6's negative controls: an fsync that must keep refusing while the
    // device refuses its cache flush, and a mid-flush redirty raced for real and
    // re-read off the image. Both bodies in `tests/common/volumes.rs`.
    ("fsync_failed_commit", Sched::Parallel, Tier::Weekly),
    ("redirty_mid_flush", Sched::Parallel, Tier::Weekly),
    // A truncate staged inside a flush's metadata window, re-read off the image.
    ("ftruncate_flush_race", Sched::Parallel, Tier::Weekly),
    // The rename gate's FAT arm, a host-side volume oracle like `fat_backing_revoked`.
    ("fs_rename_durable", Sched::Parallel, Tier::Weekly),
    // The directory work's FAT arm, `fs_rename_durable`'s oracle shape.
    ("fs_dirs_durable", Sched::Parallel, Tier::Weekly),
    ("va_exhaustion", Sched::Parallel, Tier::Weekly),
    ("heap_ceiling_bounds", Sched::Parallel, Tier::Nightly),
    ("iommu_context_absent", Sched::Parallel, Tier::Weekly),
    ("iommu_empty_domain", Sched::Parallel, Tier::Weekly),
    ("iommu_interrupt_remapping", Sched::Parallel, Tier::Weekly),
    ("iommu_virtio_platform", Sched::Parallel, Tier::Nightly),
    ("iommu_domain_isolation", Sched::Parallel, Tier::Weekly),
    ("iommu_gpu_scanout_swap", Sched::Parallel, Tier::Nightly),
    ("iommu_gpu_foreign_backing", Sched::Parallel, Tier::Weekly),
    ("iommu_hda_foreign_bdl", Sched::Parallel, Tier::Weekly),
    ("iommu_sound_foreign_dma", Sched::Parallel, Tier::Weekly),
    // The one arm of that family whose device is driven by a *process*, and
    // the only one whose verdict is that the machine is still running.
    ("userdev_dma_fault", Sched::Parallel, Tier::Nightly),
    // Two claims of a function nothing resets, the first closed with its grant
    // still mapped: the verdict is the first holder's own grant, read in the
    // guest after the second holder wrote its own.
    ("userdev_residue_is_its_own", Sched::Parallel, Tier::Nightly),
    // blockd, the NVMe driver in userland, on a second controller beside the
    // kernel's: partitions served and timed against the kernel's driver; a
    // controller reset and its own death, each survived by the client and
    // judged off the image by the host's readers; and a transfer outside what
    // its function was lent, which is a fault record. Each boot runs several
    // blockd lifetimes and one waits out a ten-second silence.
    ("blockd_serves_partitions", Sched::Parallel, Tier::Weekly),
    ("blockd_survives_its_death", Sched::Parallel, Tier::Weekly),
    ("blockd_dma_outside_the_lent", Sched::Parallel, Tier::Weekly),
    // What a claim may lend: a kernel driver's pool refused, the claim's bound
    // refusing the next region at the count it leaves room for, and lending and
    // taking back ten narrowed domains' worth of addresses with the kernel
    // standing; then, on a second boot, a function no release resets is never
    // lent where it was left aimed.
    ("blockd_lends_within_its_bound", Sched::Parallel, Tier::Weekly),
    // Two live HDA links, refused by name: the negative control on the kernel's
    // bind path. No sample is played; lines are the verdict.
    ("hda_two_live_refused", Sched::Parallel, Tier::Weekly),
];

/// The test binaries a [`MACHINE_TESTS`] or [`SCREEN_TESTS`] entry runs, which
/// are all its boots carry of the suite's catalogue: **ROOT is held whole in
/// the guest's memory**, so a binary a boot does not run is memory the guest
/// pays for nothing, twelve guests at a time.
///
/// A name with no row carries none. A grouped boot carries its members' union;
/// what a named binary spawns or links is carried with it
/// ([`qemu::carrying`]). A `run` of a binary the boot does not carry panics
/// naming this table, and a row naming what the suite did not build panics
/// before the boot.
const CARRIES: &[(&str, &[&str])] = &[
    // Each stages its own machine and carries no test binary: what is under
    // test is the image itself.
    ("update_boots_the_new_kernel", &[]),
    ("update_refusals_boot_the_other_slot", &[]),
    ("update_falls_back_from_a_dying_kernel", &[]),
    ("update_hang_kills_an_unproven_image", &[]),
    ("update_grant_refuses_a_stray_partition", &[]),
    ("update_floor_is_the_images_own", &[]),
    ("update_refused_pass_credits_no_image", &[]),
    ("blocking_read_window", &["test_rs_blocking_read_stress"]),
    ("user_copy_races_munmap", &["test_rs_copy_out_races_munmap"]),
    ("tls_rebase_window", &["test_rs_tls_dtv_race"]),
    ("writeback_reopen", &["test_rs_writeback_reopen"]),
    ("writeback_spawn", &["test_rs_writeback_spawn"]),
    ("xhci_second_controller", &["test_rs_input_events"]),
    ("xhci_msi_only", &["test_rs_input_events"]),
    ("metal_sim_input", &["test_rs_input_events"]),
    ("xhci_flap", &["test_rs_input_events"]),
    ("nvme_large_device", &["test_rs_nvme_home_roundtrip"]),
    ("va_exhaustion", &["test_rs_va_exhaustion"]),
    ("readdir_bound", &["test_rs_readdir_bound"]),
    ("mkdir_cap", &["test_rs_mkdir_cap"]),
    ("fpu_isolation", &["test_rs_fpu_isolation"]),
    ("gsbase_locked", &["test_rs_gsbase_locked"]),
    ("sched_check_build", &["test_rs_sched_stress"]),
    ("short_sleep_livelock", &["test_rs_abuse_short_sleep"]),
    ("heap_ceiling_bounds", &["test_rs_heap_ceiling"]),
    ("cache_eviction", &["test_rs_cache_eviction"]),
    ("irq_census_conservation", &["test_rs_std_mmap"]),
    ("i8042_health", &["test_rs_i8042_keyboard"]),
    ("i8042_fadt_denial", &["test_rs_i8042_keyboard"]),
    ("i8042_kbd_echo", &["test_rs_i8042_keyboard"]),
    ("i8042_undecoded_bytes", &["test_rs_i8042_keyboard"]),
    ("i8042_no_spurious_wake", &["test_rs_i8042_keyboard"]),
    ("i8042_mouse", &["test_rs_i8042_mouse"]),
    ("swiss_german_layout", &["test_rs_locale_gate"]),
    ("locale_detect", &["test_rs_locale_gate"]),
    ("locale_detect_unrecognized", &["test_rs_locale_gate"]),
    ("netd_connection_caps", &["test_rs_netd_caps"]),
    ("netd_listener_forgery", &["test_rs_netd_listener_forgery"]),
    ("netd_slow_reader", &["test_rs_netd_slow_reader"]),
    ("netd_refused_pipes", &["test_rs_netd_refused_pipes"]),
    ("netd_refused_accept", &["test_rs_netd_refused_accept"]),
    ("netd_held_open", &["test_rs_netd_held_open"]),
    ("netd_udp_refused", &["test_rs_netd_udp_refused"]),
    ("netd_udp_any_address", &["test_rs_netd_udp_any_address"]),
    ("netd_lookup_let_go", &["test_rs_netd_lookup_let_go"]),
    ("netd_hostile_peer", &["test_rs_netd_hostile_peer"]),
    ("launcher_refusals", &["test_rs_launcher_refusals"]),
    ("spawn_cwd", &["test_rs_spawn_cwd"]),
    ("input_claim_absent", &["test_rs_input_absent"]),
    ("gpu_set_resolution", &["test_rs_gpu_set_resolution"]),
    ("iommu_gpu_scanout_swap", &["test_rs_gpu_scanout_swap"]),
    ("userdev_dma_fault", &["test_rs_log_origin"]),
    ("userdev_residue_is_its_own", &["test_rs_userdev_residue"]),
    ("blockd_serves_partitions", &["test_rs_blockd_io"]),
    ("blockd_survives_its_death", &["test_rs_blockd_io"]),
    ("blockd_dma_outside_the_lent", &["test_rs_blockd_io"]),
    ("blockd_lends_within_its_bound", &["test_rs_blockd_io"]),
    (
        "inspect_reads_its_owners",
        &["test_rs_inspect_denied", "test_rs_inventory_bounds"],
    ),
    ("metal_sim_compositor", METAL_SIM_CLIENTS),
    ("metal_sim_scanout_wc", METAL_SIM_CLIENTS),
    ("metal_sim_window_caps", METAL_SIM_CLIENTS),
    ("metal_sim_ipc_hostile_peer", METAL_SIM_CLIENTS),
    ("metal_sim_compositor_stall", METAL_SIM_CLIENTS),
    ("metal_sim_client_death", METAL_SIM_CLIENTS),
    ("metal_sim_window_drag", &["test_rs_window_drag"]),
    ("metal_sim_hostile_clipboard", &["test_rs_compositor_hostile_clipboard"]),
    ("desktop_window_child", &["test_rs_window_child"]),
    ("toolkit_window_wake", &["test_rs_window_wake"]),
    ("toolkit_winit_loop", &["test_rs_winit_loop"]),
    ("toolkit_winit_pace", &["test_rs_winit_pace"]),
    ("doom_frames", &["test_rs_doom_frames"]),
    ("smp_failed_ap_leaves_no_hole", &["test_rs_smp_hole_shootdown"]),
    ("sshd_exec", &["test_rs_empty_dir_stat"]),
    ("sshd_files", &["test_rs_empty_dir_stat"]),
    ("sshd_key_auth", &["test_rs_empty_dir_stat"]),
    ("https_tls13", &["test_rs_https_fetch"]),
    ("https_tls13_e1000e", &["test_rs_https_fetch"]),
    ("pkg_install_gbae", &["test_rs_pkg_launch_gbae"]),
    ("apps_and_home_are_one_filesystem", &["test_rs_hierarchy_paths"]),
    ("layout_fresh_boot", &["test_rs_layout_paths"]),
    ("broken_data_volume_is_absent", &["test_rs_home_absent"]),
    ("data_candidate_with_bad_geometry_is_absent", &["test_rs_home_absent"]),
    ("home_overwrite_reads_back", &["test_rs_home_overwrite_zero"]),
    ("boot_volume_metadata_error", &["test_rs_boot_volume_metadata_error"]),
    ("esp_filesystem", &["test_rs_esp_files"]),
    ("log_flush_retry", &["test_rs_esp_files"]),
    ("fat_backing_revoked", &["test_rs_fat_backing_revoked"]),
    ("fs_dirs_durable", &["test_rs_fs_dirs_durable"]),
    ("fs_rename_durable", &["test_rs_fs_rename_durable", "test_rs_fs_dirs_durable"]),
    ("fsync_failed_commit", &["test_rs_fsync_flush_failed"]),
    ("ftruncate_flush_race", &["test_rs_ftruncate_flush_race", "test_rs_fs_rename_durable"]),
    ("log_backing_read_error", &["test_rs_log_volume_reread"]),
    ("redirty_mid_flush", &["test_rs_redirty_mid_flush"]),
    ("writeback_durability", &["test_rs_writeback_durability"]),
    ("kernel_log_file", &["test_rs_writeback_durability"]),
    ("double_fault_stack", &["test_rs_test_panic_child"]),
    ("idle_stack_guard", &["test_rs_test_panic_child"]),
    ("syscall_panic_halts", &["test_rs_test_panic_child"]),
    ("syscall_fault_halts", &["test_rs_test_panic_child"]),
    ("lock_across_switch_halts", &["test_rs_test_panic_child"]),
    ("heap_over_ceiling_halts", &["test_rs_test_panic_child"]),
    ("dump_left_pending_is_owed", &["test_rs_dump_stage_load"]),
    ("syscall_window_nmi", &["test_rs_nmi_window_spin"]),
    ("syscall_window_nmi_controls", &["test_rs_nmi_window_spin"]),
    ("partition_claim", &["test_rs_partition_claimant"]),
    ("partition_claim_gives_up", &["test_rs_partition_claimant"]),
    ("partition_claim_departure", &["test_rs_partition_claimant"]),
    ("log_program_line", &["test_rs_log_origin"]),
    ("log_program_forgery", &["test_rs_log_forger"]),
    ("log_after_a_refused_stop", &["test_rs_log_refused_stop"]),
    ("log_resume_meets_its_flush", &["test_rs_log_refused_stop"]),
    ("log_ring_keeps_the_owners_slots", &["test_rs_log_flood"]),
    ("log_program_flood", &["test_rs_log_flood"]),
    ("log_program_line_after_its_records", &["test_rs_log_hold"]),
    ("log_carrier_forgery", &["test_rs_log_carrier_forger"]),
    ("log_stream", &["test_rs_log_origin", "test_rs_empty_dir_stat"]),
    ("log_stream_e1000e", &["test_rs_log_origin", "test_rs_empty_dir_stat"]),
    ("log_stream_stalled_reader", &["test_rs_log_flood"]),
    ("c_capture_ignores_daemon_lines", &["test_c_71_macro_empty_arg"]),
    ("quiesce_stops_the_machine", &["test_rs_quiesce_writers"]),
    ("quiesce_refuses_a_second_shutdown", &["test_rs_quiesce_twice"]),
    ("quiesce_wakes_on_the_last_park", &["test_rs_quiesce_last"]),
    ("quiesce_wakes_on_the_last_teardown", &["test_rs_quiesce_last"]),
    ("quiesce_leaves_the_volume_whole", &["test_rs_quiesce_fsync"]),
    ("swap_crash_rolls_back", &["test_rs_swap_crash"]),
    ("swap_quiets_the_function", &["test_rs_swap_claim_idle"]),
    ("swap_keeps_what_nothing_reset", &["test_rs_swap_claim_running"]),
    ("swap_fault_tells_its_holder", &["test_rs_swap_claim_astray"]),
    ("swap_resets_the_function", &["test_rs_swap_flr_probe"]),
    ("swap_not_inherited", &["test_rs_swap_probe"]),
    ("wall_clock_rtc_dead", &["test_rs_wall_clock_now"]),
    ("wall_clock_rtc_unstable", &["test_rs_wall_clock_now"]),
    ("wall_clock_no_century", &["test_rs_wall_clock_now"]),
    ("wall_clock_century_register", &["test_rs_wall_clock_now"]),
    ("wall_clock_utc", &["test_rs_wall_clock_now"]),
    ("file_mtime_survives_a_reboot", &["test_rs_file_mtime"]),
    ("file_mtime_undated", &["test_rs_file_mtime"]),
    ("screen_console_clear", &["test_rs_test_screen_graffiti"]),
    ("screen_console_scroll", &["test_rs_test_screen_churn"]),
    ("screen_console_panic", &["test_rs_test_panic_child"]),
    ("screen_fatal_halt", &["test_rs_test_panic_child"]),
    ("panic_halts_the_others_first", &["test_rs_panic_halts_first"]),
];

/// The clients every `tests/metalcase` desktop boot carries: the group shares
/// one boot, so each member's row names them all.
const METAL_SIM_CLIENTS: &[&str] = &[
    "test_rs_window_caps",
    "test_rs_ipc_hostile_peer",
    "test_rs_compositor_stall",
    "test_rs_compositor_client_death",
];

/// **The metal profile**: which registrations run on the ThinkPad T14, what
/// boots each one needs, and how each is judged off the log the stick came back
/// with.
///
/// A separate table, so a QEMU-only test simply has no row — the answer for
/// every name nobody has looked at. A [`metal::Metal::QemuOnly`] row is the
/// other answer: a name somebody *has* looked at and ruled out, with the reason.
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
        metal::Metal::Runs { arms: METALDEVICECASE, judge: |b| devices::on_metal(b[0]) },
    ),
    (
        "lan_dhcp_lease",
        metal::Metal::Runs { arms: LANCASE, judge: |b| lan::on_metal(b[0]) },
    ),
    (
        // Folded into `lan_dhcp_lease`'s judge once the PHY is brought up
        // (#453): lancase's own first-message record then carries this fact.
        "lan_message_delivery",
        metal::Metal::Runs { arms: LANICSCASE, judge: |b| lan::provoked_on_metal(b[0]) },
    ),
    (
        // The first byte: a lease from the bench's own router, read off the
        // stick, while the host pings the address this machine had before.
        "lan_lease_report",
        metal::Metal::Runs { arms: LANLEASECASE, judge: |b| lan::leased_on_metal(b[0]) },
    ),
    (
        "lan_talk",
        metal::Metal::Runs { arms: LANTALKCASE, judge: |b| lan::talked_on_metal(b[0]) },
    ),
    (
        // netd swapped for the build's own binary while the boot runs, by a
        // second `toyos-metal --swap` beside the flashing one; the stick's
        // `/log` is the oracle that nothing rebooted between the two netds.
        "lan_swap",
        metal::Metal::Runs { arms: LANSWAPCASE, judge: |b| common::swap::swapped_on_metal(b[0]) },
    ),
    // ---- one image: tests/testcases, no parameters, one job list ----
    (
        "blackbox_unclaimed_page",
        metal::Metal::Runs { arms: TESTCASES, judge: |b| power::blackbox_unclaimed(&b[0].loader(), &b[0].kernel()) },
    ),
    (
        // The machine's own CPU count, off the SMP bring-up records — a source
        // independent of the `control_regs:` lines it is then held to. The QEMU
        // registration says four because the harness staged four; here the
        // laptop says how many it has.
        "control_regs",
        metal::Metal::Runs {
            arms: TESTCASES,
            judge: |b| control_regs(b[0].kernel().text(), b[0].cpus()?),
        },
    ),
    (
        "ioapic_topology",
        metal::Metal::Runs { arms: TESTCASES, judge: |b| ioapic_topology(b[0].kernel().text()) },
    ),
    (
        "klogd_hosted",
        metal::Metal::Runs { arms: TESTCASES, judge: |b| klogd_hosted(&b[0].kernel()) },
    ),
    (
        // The stimulus a host types at a console in QEMU is this boot's own job
        // list on the T14: every job that runs and exits is a process exit, and
        // the census is printed at each one.
        "irq_census_conservation",
        metal::Metal::Runs {
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
        metal::Metal::Runs { arms: TESTCASES, judge: |b| log_close_survived(b[0]) },
    ),
    (
        "mkdir_cap",
        metal::Metal::Runs {
            arms: TESTCASES_MKDIR,
            judge: |b| b[0].job_passed("test_rs_mkdir_cap"),
        },
    ),
    (
        "readdir_bound",
        metal::Metal::Runs {
            arms: TESTCASES_READDIR,
            judge: |b| b[0].job_passed("test_rs_readdir_bound"),
        },
    ),
    (
        "short_sleep_livelock",
        metal::Metal::Runs {
            arms: TESTCASES,
            // A livelocked CPU produces no exit record at all, which is the
            // whole verdict: the defect this is aimed at was caught twice by NMI
            // on this very machine.
            judge: |b| b[0].job_passed("test_rs_abuse_short_sleep"),
        },
    ),
    (
        "wake_storm_cost",
        metal::Metal::Runs {
            arms: TESTCASES,
            judge: |b| b[0].job_passed("test_rs_wake_storm_cost"),
        },
    ),
    (
        // The shipped tone client, twice in series, plays to completion and
        // exits 0, and soundd names how each left.
        "shipped_client_departures",
        metal::Metal::Runs {
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
        metal::Metal::Runs {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_audio_idle_suspend")?;
                audio::idle_suspend_on_metal(&b[0].log())
            },
        },
    ),
    (
        "hda_tone",
        metal::Metal::Runs {
            arms: TESTCASES,
            judge: |b| {
                b[0].job_passed("test_rs_audio_tone")?;
                audio::tone_on_metal(&b[0].log())
            },
        },
    ),
    (
        "hda_client_stall",
        metal::Metal::Runs {
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
        metal::Metal::Runs {
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
        metal::Metal::Runs {
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
        metal::Metal::Runs {
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
        metal::Metal::Runs {
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
        metal::Metal::Runs {
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
        metal::Metal::Runs {
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
        metal::Metal::Runs {
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
        metal::Metal::Runs {
            arms: TESTCASES,
            judge: |b| smp_roster_and_tsc_trail(b[0].kernel().text(), b[0].cpus()?),
        },
    ),
    (
        "pmm_accounting",
        metal::Metal::Runs { arms: TESTCASES, judge: |b| pmm_accounting(b[0].kernel().text()) },
    ),
    (
        "acpi_table_inventory",
        metal::Metal::Runs {
            arms: TESTCASES,
            judge: |b| acpi_table_inventory(b[0].kernel().text()),
        },
    ),
    (
        "timer_calibration",
        metal::Metal::Runs {
            arms: TESTCASES,
            judge: |b| {
                timer_calibration(b[0].kernel().text())?;
                tsc_agrees_with_cpuid(b[0].kernel().text())
            },
        },
    ),
    (
        "pci_inventory",
        metal::Metal::Runs { arms: TESTCASES, judge: |b| pci_inventory(b[0].kernel().text()) },
    ),
    // ---- one image: tests/latencycase armed with the shootdown bench ----
    (
        "tlb_shootdown_cost",
        metal::Metal::Runs {
            arms: LATENCYCASE,
            // The machine's own CPU count, off the bring-up records rather than
            // off a number the harness staged: the QEMU registration says eight
            // because it asked for eight, and here the laptop says how many it
            // has.
            judge: |b| {
                let (p50, p99) = tlb_shootdown_cost(b[0].kernel().text(), b[0].cpus()?)?;
                b[0].measured("tlb.latencycase.p50_ns", p50);
                b[0].measured("tlb.latencycase.p99_ns", p99);
                Ok(())
            },
        },
    ),
    (
        "latency_wake",
        metal::Metal::Runs {
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
        metal::Metal::Runs {
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
        metal::Metal::Runs { arms: JOBCASE, judge: |b| power::done_chain(&b[0].after_the_reset()?) },
    ),
    (
        // Its own boot, and it must not share one: it is the only arm in this
        // profile that deliberately leaves the machine unable to end its own
        // boot, and what it judges is that the machine ended it anyway.
        "boot_deadline_ends_a_wedge",
        metal::Metal::Runs {
            arms: &[metal::once("deadlinewedge", "tests/jobcase", &["wedge-before-reset"], &[])],
            judge: |b| power::deadline_wedge_chain(&b[0].kernel(), &b[0].after_the_reset()?),
        },
    ),
    (
        // One boot: a machine writing to the stick continuously, reset out from
        // under itself by the deadline with the controller mid-transfer, and
        // the stick enumerable on the next host afterwards.
        // `boot.usbload.stick_secs` is that, refused by the loop before this
        // judge runs.
        "usb_reset_records_the_phase_it_cut",
        metal::Metal::Runs {
            arms: &[metal::once("usbload", "tests/jobcase", &["usb-reset-under-load"], &[])],
            judge: |b| power::usb_load_chain(&b[0].kernel(), &b[0].after_the_reset()?),
        },
    ),
    (
        // Its own boot: the first WRITE(10) the boot stick takes is abandoned
        // mid-flight, and what is judged is the one thing QEMU's `usb-storage`
        // cannot answer — whether a device holding a toggle, a sequence number
        // and half a command comes back from the class's Reset Recovery on the
        // machine's own controller.
        "usb_transport_break",
        metal::Metal::Runs {
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
        metal::Metal::Runs {
            arms: &[metal::once("hardlockup", "tests/jobcase", &["hard-lockup-probe"], &[])],
            judge: |b| power::hard_lockup_chain(&b[0].kernel(), &b[0].after_the_reset()?),
        },
    ),
    (
        // Its own boot, and it must not share one: it deliberately leaves the
        // page holding a record no stick owns, and a boot that then read it as
        // a predecessor's is exactly what the arm above judges.
        // Its own boot: it deliberately leaves the page holding a record no
        // stick owns, and a boot that then read it as a predecessor's is the
        // defect. The T14 runs the same three passes QEMU does — the loader
        // points `BootNext` at itself, so they are one flash.
        "blackbox_foreign_record",
        metal::Metal::Runs {
            arms: &[metal::once(
                "foreignrecord",
                "tests/jobcase",
                &["blackbox-foreign-identity"],
                &[],
            )],
            judge: |b| {
                let after = b[0].after_the_reset()?;
                let said = after.must_say("record another image left in this memory")?.to_string();
                // Named and cleared, and never reported as this stick's own.
                power::says_nothing_of(&after, bootlog::PREVIOUS_PANIC)?;
                power::says_nothing_of(&after, "the last boot read")?;
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
        metal::Metal::Runs {
            arms: JOBCASE,
            judge: |b| {
                power::reset_register_decoded(&b[0].kernel())?;
                b[0].kernel().must_say(bootlog::REBOOTING).map(|_| ())
            },
        },
    ),
    // ---- one image: tests/metalcase ----
    (
        "metal_sim_scanout_wc",
        metal::Metal::Runs { arms: METALCASE, judge: |b| scanout_wc(b[0].kernel().text()) },
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
        metal::Metal::Runs { arms: SELFTESTS, judge: |b| pci_cap_selftest(b[0].kernel().text()) },
    ),
    (
        "process_reopen_selftest",
        metal::Metal::Runs { arms: SELFTESTS, judge: |b| process_reopen(b[0].kernel().text()) },
    ),
    (
        // **Two of its three probes pass here and the third cannot run.**
        // Measured on a metal-shaped guest: the two `revoke-selftest` probes
        // both PASS, and `pc-unbind-selftest` prints `FAIL (this boot has no
        // metadata page cache)` — the slot it needs is one the virtio machine's
        // NVMe home has and the T14's USB-only volumes do not. Judging the two
        // that do run would be a different test under the same name, so the
        // whole registration stays where it can answer for all three.
        "read_fault_selftests",
        metal::Metal::QemuOnly(
            "`pc-unbind-selftest` reports `FAIL (this boot has no metadata page cache)` on a \
             machine whose volumes are all on the boot stick; the two `revoke-selftest` probes \
             beside it pass, and splitting them into a name of their own is what would put that \
             half on the machine",
        ),
    ),
    (
        "leak_rollback_selftest",
        metal::Metal::Runs { arms: SELFTESTS, judge: |b| leak_rollback(b[0].kernel().text()) },
    ),
    (
        "lapic_spurious_vector",
        metal::Metal::Runs { arms: SELFTESTS, judge: |b| lapic_vectors(b[0].kernel().text()) },
    ),
    (
        // The T14's own controller publishes a real capability list, which is
        // the half of this QEMU cannot give: q35's nec-usb-xhci has no USB
        // Legacy Support capability in it at all.
        "xhci_xecp_walk",
        metal::Metal::Runs { arms: SELFTESTS, judge: |b| xhci_xecp(b[0].kernel().text()) },
    ),
    (
        // Same: the crafted nine are the point, and beside them the parser
        // binds a boot stick off a descriptor a real controller delivered.
        "xhci_descriptor_walk",
        metal::Metal::Runs { arms: SELFTESTS, judge: |b| xhci_descriptors(b[0].kernel().text()) },
    ),
    (
        // No drain on this side: the whole boot's records are on the stick, so
        // the probe's line is either in them or it never ran.
        "sysret_ss_reload",
        metal::Metal::Runs { arms: SELFTESTS, judge: |b| sysret_ss(b[0].kernel().text()) },
    ),
    (
        "input_merge",
        metal::Metal::Runs { arms: SELFTESTS, judge: |b| input_merge_ok(b[0].kernel().text()) },
    ),
    (
        "operation_nesting",
        metal::Metal::Runs {
            arms: SELFTESTS,
            judge: |b| operation_nesting_log(b[0].kernel().text()),
        },
    ),
];

/// **The [`METAL`] rows no QEMU registration answers for**, each with why none
/// can: a verdict under a name only the T14 reports.
const METAL_ONLY: &[(&str, &str)] = &[
    (
        "lan_message_delivery",
        "whether the T14's own I219 delivers a message through that machine's interrupt \
         remapping is a fact of that part and that path; QEMU's e1000e is another part behind \
         another path",
    ),
    (
        "wake_storm_cost",
        "its verdict is that a wake storm's cost grows linearly with the waiters, read off the \
         TSC around the syscall, and a guest's TSC runs while its host has the vCPU",
    ),
    ("shipped_client_departures", AUDIO_ON_METAL_ONLY),
    ("audio_idle_suspend", AUDIO_ON_METAL_ONLY),
    ("hda_tone", AUDIO_ON_METAL_ONLY),
    ("hda_client_stall", AUDIO_ON_METAL_ONLY),
    ("soundd_log_stall", AUDIO_ON_METAL_ONLY),
    (
        "tlb_shootdown_cost",
        "its product is how long a machine-wide shootdown takes, a span a guest's host \
         sets",
    ),
    (
        "latency_wake",
        "its product is how late a programmed wake lands, a span a guest's host sets",
    ),
    (
        "syscall_cost",
        "its product is a cycle count per syscall and the clock rate beside it, which a \
         guest's host sets",
    ),
    (
        "tlb_shootdown_waits",
        "its verdict is a lower bound on a span the guest reads off its own clock around the \
         syscall",
    ),
    (
        "dump_nmi_probe",
        "its verdict rests on a CPU the actuator deafens for a window of its own clock being \
         kicked and probed inside it, which a starved guest misses",
    ),
    (
        "watchdog_fed",
        "its verdict is that a feeding kernel is not reset, which only a span of the \
         machine's own time can show",
    ),
];

/// Why an audio row has no QEMU arm.
const AUDIO_ON_METAL_ONLY: &str = "audio is judged on the T14 and in no QEMU guest (owner ruling)";

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
/// count. The three that do change the machine are not here:
/// `no-ap-control-regs` leaves an AP without them, `smp-skip-ap` leaves one
/// out and `test-tiny-va` shrinks the address space, and each would be
/// answering for the boot every other row on it read.
const SELFTESTS: &[metal::Arm] = &[metal::once(
    "selftests",
    "tests/testcases",
    &[
        "pci-cap-selftest",
        "process-reopen-selftest",
        // `revoked-backing-selftest` and `pc-unbind-selftest` are deliberately
        // absent: `read_fault_selftests` is the only rider they had and it is
        // declared QEMU-only above, so arming them here would put a `FAIL` line
        // on the stick that no verdict claims.
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

/// The shared block's Rust binaries that do **not** go on the T14, and what
/// each one's boot said when it was tried there.
///
/// Every row is a measurement, not an inheritance: the whole discovered set was
/// staged onto one metal-shaped image and booted, and these are the names whose
/// exit record was not `code=0`. A name here with a reason that has stopped
/// being true is a name that should come off — the boot is the judge, and it is
/// cheap to re-run.
///
/// The eight `SYS_DEBUG` binaries are not here: they ride
/// [`SHARED_METAL_DEBUG`], which is the same boot on the kernel that carries
/// the syscall they call. The track's ruling is that the metal profile flashes
/// test images, so that kernel may go on the stick.
const METAL_SKIP: &[(&str, &str)] = &[];

/// The boot the shared block's Rust binaries ride on the T14.
///
/// **`Profile::Headless`'s virtio machine is not what the T14 is**, so the
/// audit's list of names "bound to the virtio machine" is a hypothesis about
/// this boot rather than a fact about it. It is settled by booting them: what
/// [`METAL_SKIP`] holds is what the machine refused, and nothing is excluded for
/// a shape it was never tried on.
fn shared_metal(
    rust_bins: &[(String, Vec<u8>)],
    keep: impl Fn(&str) -> bool,
) -> Vec<metal::SharedBoot> {
    let skipped: BTreeSet<&str> = METAL_SKIP.iter().map(|(name, _)| *name).collect();
    let discovered = discover_rust_tests(rust_bins);
    for (name, _) in METAL_SKIP {
        assert!(
            discovered.iter().any(|d| d == name),
            "METAL_SKIP names {name:?}, which the shared block does not discover; a row for a \
             binary that is gone excludes nothing and hides that it is gone"
        );
    }
    let (debug, shipping): (Vec<String>, Vec<String>) = discovered
        .into_iter()
        .filter(|name| !skipped.contains(name.as_str()))
        .partition(|name| ACTUATOR_TESTS.contains(&name.as_str()));
    vec![
        metal::SharedBoot {
            boot: "shared".to_string(),
            config: "tests/testcases",
            params: &[],
            features: &[],
            members: 38,
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
            members: 18,
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
    let dir = compile::testcases_dir();
    let mut jobs = Vec::new();
    let mut files = Vec::new();
    let mut links = Vec::new();
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    for (case, data) in c_bins {
        if !keep(case) || C_METAL_SKIP.iter().any(|(name, _)| name == case) {
            continue;
        }
        let expect = dir.join(format!("{case}.expect"));
        // A case with no committed expectation is one nothing could judge, and
        // shipping it would be a job that passes by comparing nothing.
        let Ok(expected) = fs::read(&expect) else { continue };
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
        files.push((format!("expect/{case}"), expected));
        files.push((format!("bin/test_c_{case}"), data.clone()));
        links.push((format!("bin/{case}"), format!("/system/bin/{CCHECK}")));
        jobs.push(case.clone());
    }
    metal::SharedBoot {
        boot: "ccorpus".to_string(),
        config: "tests/testcases",
        params: &[],
        features: &[],
        members: 90,
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
    back.kernel().must_say(bootlog::REBOOTING).map(|_| ())
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
        case: "83_utf8_in_identifiers",
        stage: Stage::Built,
        why: Why::Declined("its identifiers are UTF-8 and so is its `printf` format, and libc's `printf` writes each byte of a format as a character of its own (issues/build/libc-printf-re-encodes-every-non-ascii-byte-of-its-format.md): `привет` arrives as `Ð¿Ñ\u{80}Ð¸Ð²ÐµÑ\u{82}`"),
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
        case: "124_atomic_counter",
        stage: Stage::Refused("unknown type name 'uint_least16_t'"),
        why: Why::Declined("C11 atomics: clang's `stdatomic.h` needs the `least` types `stdint.h` does not define"),
    },
    NotRun {
        case: "125_atomic_misc",
        stage: Stage::Refused("unknown type name 'uint_least16_t'"),
        why: Why::Declined("C11 atomics, as 124_atomic_counter"),
    },
    NotRun {
        case: "128_run_atexit",
        stage: Stage::NoLink("on_exit"),
        why: Why::Declined("`on_exit`, a glibc extension libc does not define, and a -D per configuration to have a main at all"),
    },
    NotRun {
        case: "136_atomic_gcc_style",
        stage: Stage::Refused("unknown type name 'uint_least16_t'"),
        why: Why::Declined("C11 atomics, as 124_atomic_counter"),
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

/// Discover Rust test binaries from build output.
/// Skips shared libraries, helper binaries, and audio tests (dedicated boot).
///
/// **A name that arrives this way is registered by nothing but its file.**
/// `tests/toyos-rust-tests/src/bin/<name>.rs` is the whole declaration — no row
/// here names it.
fn discover_rust_tests(bins: &[(String, Vec<u8>)]) -> Vec<String> {
    let mut names: Vec<String> = bins
        .iter()
        .filter_map(|(name, _)| {
            if name.ends_with(".so") {
                return None;
            }
            if RUST_SKIP.contains(&name.as_str()) {
                return None;
            }
            Some(name.clone())
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

/// How many kernel lines a dead test's report is printed with.
///
/// A fault report is a header, a register dump and a bounded backtrace, and the
/// daemons keep talking beside it. Sixty holds one whole report and its
/// surroundings; past that the tail is the part that says how it ended, and the
/// line says how many it dropped rather than dropping them in silence.
const MAX_KERNEL_LINES: usize = 60;

/// The kernel's own account of a test that died, which `stdout` cannot carry.
///
/// **`exit code Some(-1)` is the kernel saying it killed the process** —
/// `fatal_exception` answers a Ring 3 fault with `kill_process(-1)` — and every
/// word of *why* is a `log!`: the vector, `rip`, `cr2`, the resolved symbol.
/// `run_test_paced` files kernel lines under `serial` and keeps them out of
/// `stdout`, which is right for a test that passed and leaves a killed one with
/// no evidence whatsoever. `std_unwind` and `std_unwind_so` have been red in
/// every twelve-shard CI run there has been, and each one reported the same
/// eleven characters and no address.
fn kernel_account(result: &TestResult) -> String {
    let lines: Vec<&str> = result.serial.lines().filter(|l| qemu::is_kernel_line(l)).collect();
    if lines.is_empty() {
        return "\n--- the kernel said nothing while it ran ---".to_string();
    }
    let dropped = lines.len().saturating_sub(MAX_KERNEL_LINES);
    let how_many = if dropped > 0 {
        format!(" (the last {MAX_KERNEL_LINES} of {})", lines.len())
    } else {
        String::new()
    };
    format!("\n--- what the kernel said{how_many} ---\n{}", lines[dropped..].join("\n"))
}

fn check_c_result(result: &TestResult) -> bool {
    let test_name = result.name.strip_prefix("test_c_").unwrap_or(&result.name);

    if let Some(err) = &result.error {
        eprintln!("FAIL c::{test_name}: {err}{}", kernel_account(result));
        return false;
    }

    match result.exit_code {
        Some(0) => {
            let expect_file = compile::testcases_dir().join(format!("{test_name}.expect"));
            if expect_file.exists() {
                // TinyCC's runner captured its warnings about the case with its output.
                let warned = format!("{test_name}.c:");
                let expected: String = fs::read_to_string(&expect_file)
                    .unwrap()
                    .lines()
                    .filter(|l| !(l.starts_with(&warned) && l.contains(": warning: ")))
                    .map(|l| format!("{l}\n"))
                    .collect();
                // **The one comparison in this suite that reads a whole capture
                // as one program's output, on a console every process shares.**
                // `common::console::verdict` takes the lines that are some
                // other process's out of it first, and hands them back so they
                // can be printed rather than vanish. Its own doc has the two
                // ways that happens and `c_capture_ignores_daemon_lines` is the
                // gate under it.
                let verdict = common::console::c_verdict(&result.stdout, &expected);
                if !verdict.filtered.is_empty() {
                    eprintln!(
                        "  [c] {test_name}: {} console line(s) in this window were another \
                         process's and did not decide the verdict:\n    {}",
                        verdict.filtered.len(),
                        verdict.filtered.join("\n    "),
                    );
                }
                if let Some(mismatch) = verdict.mismatch {
                    eprintln!("FAIL c::{test_name}: {mismatch}");
                    return false;
                }
            }
            true
        }
        Some(code) => {
            eprintln!(
                "FAIL c::{test_name}: exit code {code}\nstdout: {}{}",
                result.stdout,
                kernel_account(result)
            );
            false
        }
        None => {
            eprintln!("FAIL c::{test_name}: no exit code{}", kernel_account(result));
            false
        }
    }
}

/// Every red prints the test's own lines, a ceiling's too: a hang is named by
/// the last thing the test said.
fn check_rust_result(result: &TestResult) -> bool {
    let test_name = result.name.strip_prefix("test_rs_").unwrap_or(&result.name);
    let why = match (&result.error, result.exit_code) {
        (None, Some(0)) => return true,
        (Some(err), _) => err.to_string(),
        (None, Some(code)) => format!("exit code {code}"),
        (None, None) => "no exit code".to_string(),
    };
    eprintln!("FAIL rs::{test_name}: {why}\nstdout:\n{}{}", result.stdout, kernel_account(result));
    false
}

/// The kernel names the frames of a process it loaded off a **disk**.
///
/// `null_deref_run_from_disk` is this child's alone.
fn check_disk_backtrace(result: &TestResult) -> bool {
    if !check_rust_result(result) {
        return false;
    }

    let checks: &[(&str, &str)] = &[
        ("SEGFAULT tid=", "expected a SEGFAULT header for the child run off /home"),
        (
            "null_deref_run_from_disk",
            "expected the faulting function's demangled name — a process loaded off a disk \
             got a backtrace with no names in it",
        ),
    ];

    let mut ok = true;
    for (needle, msg) in checks {
        if !result.serial.contains(needle) {
            eprintln!("FAIL rs::disk_backtrace: {msg}\nserial:\n{}", result.serial);
            ok = false;
        }
    }
    ok & check_symbols_were_read("disk_backtrace", &result.serial)
}

/// No line of a crash report conceded its symbol to something it could not
/// reach.
///
/// **The reason every check above is allowed to be a `contains`.** A symbol
/// lookup on the fault path may not wait — the faulting thread may itself hold
/// whatever it would wait for — so "no name here" used to mean either "this
/// address has no name" or "nobody looked", and a gate asserting on a name red
/// intermittently on the second. It was not hypothetical: `fault_gates` red 2
/// of 5 full runs and `disk_backtrace` 1 of 5 on `wt/toyos-logd`, with the
/// backtrace three lines below the unresolved `rip:` naming the very symbol the
/// line above had lost.
///
/// `process::SymbolLookup` says which, so this reds on the reason instead. Since
/// 2026-08-22 the lookup takes no lock at all — the names come off the running
/// task's own record — so the two reasons left are a CPU inside a scheduler pass
/// and a CPU running nothing, and either one in a report is a finding rather
/// than weather.
fn check_symbols_were_read(test: &str, serial: &str) -> bool {
    const CONCEDED: &str = "<symbol unread:";
    let lines: Vec<&str> = serial.lines().filter(|l| l.contains(CONCEDED)).collect();
    if lines.is_empty() {
        return true;
    }
    eprintln!(
        "FAIL rs::{test}: the crash report could not read a symbol it was asked for, so a bare \
         address in it is a lost race and not a verdict:\n{}\nserial:\n{serial}",
        lines.join("\n"),
    );
    false
}

/// The lock-across-switch tripwire must fire, and its `panicked at` must name the syscall
/// that held the lock rather than the scheduler that caught it — which is the
/// only thing `#[track_caller]` on `assert_baseline` buys.
///
/// A whole-buffer `contains("syscall/dispatch.rs")` certifies none of that: the
/// backtrace names every frame, that file's included. Scope it instead to the
/// window between this panic's header and its message — `panicked at
/// <location>` is the only thing in there.
fn check_tripwire_attribution(serial: &str) -> Result<(), String> {
    const MSG: &str = "scheduler entered while a lock is held";
    const HEADER: &str = "PANIC:";
    let msg_at = serial
        .find(MSG)
        .ok_or("expected the lock-across-switch tripwire to fire")?;
    let header_at = serial[..msg_at]
        .rfind(HEADER)
        .ok_or("tripwire message with no panic header before it")?;
    let location = &serial[header_at..msg_at];
    if !location.contains("syscall/dispatch.rs") {
        return Err(format!(
            "expected the tripwire to name the guilty call site, not scheduler.rs; got: {}",
            location.trim()
        ));
    }
    Ok(())
}

/// The kernel's Ring 0 read of the address `test_panic_child` named halted on
/// that address as **unmapped**. A read that demand paging filled for the
/// caller re-executes into SMAP's protection fault instead, so the word is what
/// says nothing was mapped into the current process.
fn check_ring0_read_unmapped(serial: &str) -> Result<(), String> {
    const READ_OF: &str = "SYS_DEBUG: a Ring 0 read of ";
    let at = serial.find(READ_OF).ok_or("expected the kernel to name the address it read")?;
    let named = serial[at + READ_OF.len()..].split_whitespace().next().unwrap_or_default();
    let addr = named
        .strip_prefix("0x")
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
        .ok_or_else(|| format!("the address the kernel read is not a number: {named:?}"))?;
    if addr == 0 {
        return Err("expected the demand-paged window, not the null read".to_string());
    }
    let want = format!("KERNEL PANIC: read unmapped address at {addr:#x}");
    if !serial.contains(&want) {
        return Err(format!("expected `{want}`: the read did not fault as unmapped at {addr:#x}"));
    }
    Ok(())
}

/// The exit code says the child died; only the serial says *why*.
///
/// A #DE with no gate escalates to #DF, and `double_fault_handler` halts every
/// CPU — so a run that reaches this function at all already survived. What is
/// left to check is that the kernel took the fault as a #DE rather than as
/// something the escalation left behind: the report names the vector and the
/// function that raised it, and no double fault appears in the window.
fn check_fault_gates(result: &TestResult) -> bool {
    if !check_rust_result(result) {
        return false;
    }

    let checks: &[(&str, &str)] = &[
        ("SIGFPE tid=", "expected a SIGFPE header for the divide by zero"),
        ("divide error", "expected the #DE report to name the vector"),
        (
            "fault_gate_child::divide_by_zero",
            "expected the faulting function in the #DE backtrace",
        ),
        ("SEGFAULT tid=", "expected a SEGFAULT header for the null read"),
        ("fault_gate_child::read_null", "expected the faulting function in the #PF backtrace"),
    ];

    let mut ok = true;
    for (needle, msg) in checks {
        if !result.serial.contains(needle) {
            eprintln!("FAIL rs::fault_gates: {msg}\nserial:\n{}", result.serial);
            ok = false;
        }
    }
    if result.serial.contains("DOUBLE FAULT") {
        eprintln!(
            "FAIL rs::fault_gates: a Ring 3 fault escalated to #DF — its vector has no gate\
             \nserial:\n{}",
            result.serial
        );
        ok = false;
    }
    ok & check_symbols_were_read("fault_gates", &result.serial)
}

/// The guest asserts its children died; this asserts *what the kernel said* —
/// and, harder, what it did not say.
///
/// **The absences are the half the exit code cannot carry.** Vector 1 used to
/// reach a debugger-session aid that dumped registers, disarmed `DR7`/`DR6`,
/// walked a backtrace and returned to resume, and a Ring 3 process reached it
/// with one instruction. A kernel that put that handler back would still let
/// `debug_trap`'s children die if some *later* instruction faulted, so the
/// verdict has to be that the report is not in the window at all. `DOUBLE FAULT`
/// is the other absence and it is not hypothetical: without `TF` in
/// `IA32_FMASK`, `popfq` followed by `syscall` takes the `#DB` at
/// `syscall_entry+0x0` with `rsp` still the user stack, and every CPU halts.
fn check_debug_trap(result: &TestResult) -> bool {
    if !check_rust_result(result) {
        return false;
    }

    let mut ok = true;
    // `crash_report_exception`'s default arm for a Ring 3 fault, with
    // `vector_name(Vector::Debug)` after it. Matched as a whole line rather than
    // as a substring, because the binary's own name carries the word `debug`.
    let named = result
        .serial
        .lines()
        .any(|l| l.contains("FATAL tid=") && l.trim_end().ends_with(": debug"));
    if !named {
        eprintln!(
            "FAIL rs::debug_trap: no `FATAL tid=N: debug` line — the kernel ended the children \
             but not as a #DB, so the vector reached somewhere else\nserial:\n{}",
            result.serial
        );
        ok = false;
    }

    let absences: &[(&str, &str)] = &[
        (
            "DB TRAP",
            "the #DB handler's UART marker is in the window: a Ring 3 trap reached a kernel \
             report path",
        ),
        (
            "HARDWARE WATCHPOINT",
            "the watchpoint report is in the window: a Ring 3 trap made the kernel walk kernel \
             state and resume",
        ),
        (
            "DOUBLE FAULT",
            "a Ring 3 debug trap escalated to #DF — the #DB frame was built on a stack the CPU \
             could not write, which is `TF` missing from IA32_FMASK",
        ),
        (
            "KERNEL PANIC",
            "the kernel blamed itself for a trap a Ring 3 process raised",
        ),
    ];
    for (needle, msg) in absences {
        if result.serial.contains(needle) {
            eprintln!("FAIL rs::debug_trap: {msg}\nserial:\n{}", result.serial);
            ok = false;
        }
    }
    ok
}

/// Nothing to wait for: the test's own window carries everything its check
/// reads. Every name but one.
fn no_settle(_: &mut QemuInstance, _: &mut TestResult) {}

/// The trailing space is what keeps `pid=21` from matching `pid=212`.
fn accounting_of(pid: u32) -> String {
    format!("syscalls: pid={pid} ")
}

/// Select the between-the-test-and-its-check wait by name, as [`check_for`]
/// selects the check.
fn settle_for(name: &str) -> fn(&mut QemuInstance, &mut TestResult) {
    match name {
        "exit_wait_storm" => settle_exit_wait_storm,
        _ => no_settle,
    }
}

/// Select check function by test name convention.
fn check_for(name: &str) -> fn(&TestResult) -> bool {
    match name {
        "disk_backtrace" => check_disk_backtrace,
        "fault_gates" => check_fault_gates,
        "debug_trap" => check_debug_trap,
        "dlopen_dedup" => check_dlopen_dedup,
        "abuse_elf_loader" => check_abuse_elf_loader,
        "exit_wait_storm" => check_exit_wait_storm,
        _ => check_rust_result,
    }
}

/// `abuse_elf_loader` plus the reason each apply-time refusal must fire for.
///
/// Each case is refused for the right reason only if the kernel names its
/// [`toyos_elf::RelocError`] beside the file — a case refused later, for
/// another reason, would pass the exit-code check alone. Every reason is
/// checked even when the guest failed, so one run shows each case's verdict.
fn check_abuse_elf_loader(result: &TestResult) -> bool {
    use toyos_elf::RelocError;
    let mut ok = check_rust_result(result);
    let log = format!("{}{}", result.before, result.serial);
    for (file, refused, what) in [
        ("tls_apply_refs.so", RelocError::TlsOutsideSegment, "the dlopen apply-time TLS refusal"),
        ("tls_apply_spawn", RelocError::TlsOutsideSegment, "the spawn apply-time TLS refusal"),
        ("f13_refs_past.so", RelocError::TlsOutsideSegment, "the cross-module apply-time TLS refusal"),
        ("tpoff_overflow.so", RelocError::TpoffOverflows, "the dlopen TPOFF overflow"),
        ("tpoff_overflow_spawn", RelocError::TpoffOverflows, "the spawn TPOFF overflow"),
        ("globdat_past_dynsym", RelocError::SymbolPastTable, "the executable's GLOB_DAT past .dynsym"),
    ] {
        let reason = refused.as_str();
        let named = log.lines().any(|l| l.contains(file) && l.contains(reason));
        if !named {
            eprintln!(
                "FAIL rs::abuse_elf_loader: {what} did not fire for its reason — no line names \
                 {file:?} with {reason:?}{}",
                kernel_account(result)
            );
            ok = false;
        }
    }
    ok
}

/// `kernel/src/loader/tls.rs`'s `rebase_window` line for a watched spawn's
/// block the process could not yet reach before its rebase.
const TLS_BLOCK_UNREACHABLE: &str = "is not reachable before its rebase";
/// The spawns `tls_dtv_race` watches: every round of its `ROUNDS` but the first.
const TLS_RACE_WATCHED: usize = 15;

/// What a loader writes when it caches a library under the directory it searched
/// and did not find it in. Only `dlopen_dedup`'s last arm produces this string.
const FALLBACK_MISCACHED: &str = "dlopen: cached /tmp/dlopen-dedup/libtls_lib.so";

/// `dlopen_dedup` plus the half no guest can see: one library reached two ways
/// is one physical image, cached under the path that was actually opened.
fn check_dlopen_dedup(result: &TestResult) -> bool {
    if !check_rust_result(result) {
        return false;
    }
    let log = format!("{}{}", result.before, result.serial);
    if log.contains(FALLBACK_MISCACHED) {
        eprintln!(
            "FAIL rs::dlopen_dedup: {FALLBACK_MISCACHED:?} — the library was cached under the \
             directory the loader searched and did not find it in, so a later dlopen of its own \
             path mapped it a second time{}",
            kernel_account(result)
        );
        return false;
    }
    true
}

/// The name, formatted into every needle below rather than written beside a
/// `test_rs_` literal: `suite_split` reads that spelling as a machine test
/// *driving* the binary, and these only read console lines about it.
const STORM: &str = "exit_wait_storm";

/// The children `exit_wait_storm` spawns, mirrored from its `CHILDREN`.
const STORM_CHILDREN: usize = 24;

/// The calls the parent's own profile is read for, and how many of each the
/// storm is: spawn, process wait, thread join.
const STORM_CALLS: [(u64, usize); 3] = [
    (toyos_abi::syscall::SYS_SPAWN, STORM_CHILDREN),
    (toyos_abi::syscall::SYS_PROCESS_WAIT, STORM_CHILDREN),
    (toyos_abi::syscall::SYS_THREAD_JOIN, STORM_CHILDREN),
];

/// What the storm's judge reads: the window and the lines before its start
/// marker. The marker reaches the console through `logd` and the kernel's
/// records through `klogd`, so the parent's spawn record and its first
/// children's can arrive ahead of it, and the window then opens at a child's.
fn storm_log(result: &TestResult) -> String {
    format!("{}{}", result.before, result.serial)
}

/// The parent is the lowest pid among the storm's `spawn:` lines: pids are
/// never reused and it is made before any child.
fn storm_parent(log: &str) -> Option<u32> {
    let want = format!("/system/bin/test_rs_{STORM} ");
    log.lines()
        .filter(|l| l.contains("spawn: ") && l.contains(&want))
        .filter_map(|l| l.split("pid=").nth(1)?.split_whitespace().next()?.parse().ok())
        .min()
}

/// Wait for the parent's accounting line: every child's and every thread's
/// teardown line was emitted before it, so its arrival is what says the
/// capture this check reads is whole.
fn settle_exit_wait_storm(qemu: &mut QemuInstance, result: &mut TestResult) {
    let Some(pid) = storm_parent(&storm_log(result)) else { return };
    let want = accounting_of(pid);
    if storm_log(result).contains(&want) {
        return;
    }
    // A line that never comes is the ceiling's red, with its reason on the
    // result the check then answers with.
    if let Err(why) = await_guest(qemu, &mut result.serial, "the storm parent's accounting line", |c| {
        c.contains(&want)
    }) {
        result.error = Some(qemu::WaitVerdict::new(why, &[&result.before, &result.serial]));
    }
}

/// `exit_wait_storm` against the kernel's own record of the same run: the codes
/// it says the children died with, and the calls it counted the parent making.
/// The guest writes neither, so a count it reports and a publish it never asked
/// for are told apart here and nowhere else.
fn check_exit_wait_storm(result: &TestResult) -> bool {
    if !check_rust_result(result) {
        return false;
    }
    let log = storm_log(result);
    let Some(parent) = storm_parent(&log) else {
        eprintln!(
            "FAIL rs::{STORM}: no `spawn: /system/bin/test_rs_{STORM}` line reached the capture, so \
             nothing here says what the kernel saw{}",
            kernel_account(result)
        );
        return false;
    };
    let died = format!("exit: test_rs_{STORM} pid=");
    let mut codes: Vec<i32> = Vec::new();
    for line in log.lines() {
        let Some(rest) = line.split(died.as_str()).nth(1) else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let Some(Ok(pid)) = fields.next().map(str::parse::<u32>) else { continue };
        // Its children are the pids made after it.
        if pid <= parent {
            continue;
        }
        if let Some(Ok(code)) =
            fields.next().and_then(|f| f.strip_prefix("code=")).map(str::parse::<i32>)
        {
            codes.push(code);
        }
    }
    codes.sort_unstable();
    let expected: Vec<i32> = (0..STORM_CHILDREN as i32).collect();
    if codes != expected {
        eprintln!(
            "FAIL rs::{STORM}: the kernel accounted children exiting with {codes:?}, \
             against the {STORM_CHILDREN} the guest reports collecting\nstdout:\n{}{}",
            result.stdout,
            kernel_account(result)
        );
        return false;
    }
    let want = accounting_of(parent);
    let Some(line) = log.lines().find(|l| l.contains(want.as_str())) else {
        eprintln!(
            "FAIL rs::{STORM}: the kernel never accounted the parent — no `{want}` line \
             reached the capture, so nothing here says which calls it made{}",
            kernel_account(result)
        );
        return false;
    };
    for (call, least) in STORM_CALLS {
        let counted = line
            .split(&format!(" {call}="))
            .nth(1)
            .and_then(|r| r.split_whitespace().next())
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0);
        if counted < least {
            eprintln!(
                "FAIL rs::{STORM}: the parent made {counted} call(s) of syscall {call} \
                 and the storm is {least}\nstdout:\n{}{}",
                result.stdout,
                kernel_account(result)
            );
            return false;
        }
    }
    true
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

/// Everything a photograph of a frozen machine has to carry, asked of one
/// screendump.
///
/// The three summary strings are the answer; the absence of a `[page n/m]`
/// footer is what makes one photograph the *whole* answer, because Ctrl+Alt+D
/// paints once and never enters the pager — a report that needed two pages
/// would leave the verdict on one nobody can reach. And the fill is the report
/// having taken the panel rather than sitting on a client's screen, which is
/// the half `boot_checkpoint` deliberately will not do.
fn report_is_photographable(dump: &screen::Ppm, what: &str) -> Result<(), String> {
    let text = dump.text();
    for want in ["== VERDICT:", "cpu(s) answered", "== deadlines:"] {
        if !text.contains(want) {
            return Err(format!(
                "{what} does not carry {want:?}, so a photograph of this machine answers \
                 nothing\ndecoded screen:\n{text}"
            ));
        }
    }
    if let Some(row) = dump.rows().iter().find(|r| r.contains("[page ")) {
        return Err(format!(
            "{what} is paginated ({}), and nothing advances the page after Ctrl+Alt+D — so the \
             panel is a slice of the machine's log rather than the report\ndecoded screen:\n{text}",
            row.trim()
        ));
    }
    if dump.fill() != FILL_BOOT {
        return Err(format!(
            "{what} is on a panel whose fill is {:?} — this is a client's screen with kernel \
             text on it, not the report holding the panel",
            dump.fill()
        ));
    }
    Ok(())
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

/// Assert the renderer wrapped a backtrace line rather than clipping it.
///
/// The stimulus is the panic's own bottom frame: `late_panic::Nest` is a
/// generic nested in itself, so its demangled symbol is wider than any
/// console grid and its head and tail cannot share a display row. Wrap-over-
/// clip exists precisely so the symbol at the *end* of such a line survives,
/// which is why the tail is the thing asserted.
fn check_wrap(dump: &screen::Ppm) -> Result<(), String> {
    let rows = dump.rows();
    let Some(head) = dump.row_index("late_panic::Nest") else {
        return Err(format!(
            "no `late_panic::Nest` frame on screen — no over-wide symbol to wrap\n{}",
            dump.text()
        ));
    };
    if rows[head].contains("on_screen_console_check") {
        return Err(format!(
            "the frame fit one display row ({} columns); wrap is not exercised",
            rows[head].len()
        ));
    }
    // The block this frame wrapped over ends where the next frame's address
    // begins, and it is searched joined: a row count and a tail landing whole
    // inside one row are both claims about the panel's width.
    let end = rows[head + 1..]
        .iter()
        .position(|r| r.contains("0xffff"))
        .map_or(rows.len(), |n| head + 1 + n);
    if !rows[head..end].concat().contains("on_screen_console_check") {
        return Err(format!(
            "the tail of the demangled symbol never reached the screen — clipped? \
             {} row(s) of wrap before the next frame\n{}",
            end - head,
            dump.text()
        ));
    }
    Ok(())
}

/// Every row on the panel is text the log actually carries.
///
/// **The check the panel's grid owes**: `panic_console` writes only the cells
/// whose character or colour moved, so a cell it fails to write is one the
/// previous paint left standing, and past the end of a line that replaced a
/// longer one that is a string no line of the log contains.
fn check_no_stale_cells(dump: &screen::Ppm, console: &str) -> Result<(), String> {
    let said: String = console
        .replace("[kernel ", "[")
        .bytes()
        .map(|byte| match byte {
            b'\n' => '\n',
            b'\t' => ' ',
            0x20..=0x7E => byte as char,
            _ => '.',
        })
        .collect();
    for row in dump.rows() {
        let row = row.trim_end();
        if row.is_empty() || row.starts_with("[page ") || said.contains(row) {
            continue;
        }
        return Err(format!(
            "the panel row {row:?} is in no line of the log, so a cell the paint that put \
             this screen up did not write is still standing from the one before \
             it\ndecoded screen:\n{}",
            dump.text()
        ));
    }
    Ok(())
}

/// `tests/toyos-rust-tests`' binary that `tests/virtjobcase` runs as its job
/// `test_rs_abuse_readonly_copyout`.
const VIRT_COPYOUT: &str = "abuse_readonly_copyout";

/// Boot `tests/virtjobcase` under the EL2 profile and judge its job `job`:
/// it ends with exit 0, having said `said`. The kernel carries `SYS_DEBUG`
/// for `debug_refused`, and every job runs in every boot of the case.
fn virt_job(job: &str, said: &str) -> Result<(), String> {
    let config = compile::repo_root().join("tests/virtjobcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    let profile = qemu::Profile::VirtEl2;
    static COPYOUT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let copyout = COPYOUT.get_or_init(|| {
        qemu::build_toyos_bin(profile.arch(), &compile::repo_root().join("tests/toyos-rust-tests"), VIRT_COPYOUT)
    });
    let mut qemu = QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            profile,
            kernel_features: toyos_build::build::TEST_KERNEL,
            ready_marker: "control registers: SCTLR_EL1=",
            extra_root_files: vec![(format!("bin/test_rs_{VIRT_COPYOUT}"), copyout.clone())],
            ..Default::default()
        },
    );
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
    Ok(())
}

/// Boot `test_config` under the EL2 profile with the kernel selftest `armed`
/// names, and judge its one line: `<param>: PASS`.
fn virt_selftest(test_config: &Path, armed: &'static [&'static str; 1]) -> Result<(), String> {
    let [param] = armed;
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[],
        BootOptions {
            profile: qemu::Profile::VirtEl2,
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
fn run_screen_test(
    name: &str,
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    match name {
        "screen_loader_lines" => {
            // An EXCLUSIVE open of `GraphicsOutput` calls `Stop` on the
            // firmware's graphics console, so with one the panel stops at the
            // GOP query and every later loader line is on serial alone.
            let dump_at = |marker: &'static str| -> Result<(usize, String), String> {
                let options = BootOptions {
                    profile: qemu::Profile::Metal,
                    qmp: true,
                    ready_marker: marker,
                    ..Default::default()
                };
                metal_sim_argv_check(&qemu::profile_argv(&options))?;
                let mut qemu =
                    QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
                let console = qemu.boot_log().to_string();
                let dump = qemu.screendump();
                // A row that decodes in the kernel's font is a row the kernel
                // drew. Refused rather than counted: its rows are not the
                // loader's and would only push the growth below green.
                if dump
                    .rows()
                    .iter()
                    .any(|row| !row.trim().is_empty() && !row.contains(screen::UNKNOWN))
                {
                    return Err(format!(
                        "the kernel had already repainted the panel at {marker:?}, so these are \
                         its rows and not the loader's\ndecoded screen:\n{}",
                        dump.text()
                    ));
                }
                Ok((dump.text_row_bands()?, console))
            };

            let (before, at_query) = dump_at(bootlog::LOADER_GOP_LINE)?;
            let (after, console) = dump_at(bootlog::LOADER_LAST_LINE)?;

            // The growth below subtracts one boot's rows from the other's, and
            // says nothing unless the two printed the same number of lines
            // before the query. The lines themselves are not compared: each
            // boot builds its own image, so two of them carry partition GUIDs
            // drawn fresh.
            let upto = |text: &str| {
                text.lines().take_while(|line| !line.contains(bootlog::LOADER_GOP_LINE)).count()
            };
            if upto(&at_query) != upto(&console) {
                return Err(format!(
                    "one boot printed {} lines before the GOP query and the other {}, so their \
                     row counts are not each other's baseline\n--- first\n{at_query}\n--- \
                     second\n{console}",
                    upto(&at_query),
                    upto(&console)
                ));
            }

            // What the loader printed after the query, off its own console.
            let lines: Vec<&str> = console.lines().collect();
            let at = |line: &str| {
                lines
                    .iter()
                    .position(|seen| seen.contains(line))
                    .ok_or_else(|| format!("the loader never printed {line:?}\n{console}"))
            };
            let (query, last) =
                (at(bootlog::LOADER_GOP_LINE)?, at(bootlog::LOADER_LAST_LINE)?);
            if last <= query {
                return Err(format!(
                    "the console carries {:?} at line {last} and {:?} at line {query}, so there \
                     is nothing between them",
                    bootlog::LOADER_LAST_LINE,
                    bootlog::LOADER_GOP_LINE
                ));
            }
            let printed = last - query;
            // The rows those lines take on the firmware's console, whose glyph is
            // eight pixels wide (UEFI 2.11 §12.9, `EFI_GLYPH_WIDTH`): a line
            // wider than the mode's columns — the root bridges' descriptor dump
            // is one — wraps onto a row per width it fills.
            let columns = lines[query]
                .split("GOP: mode ")
                .nth(1)
                .and_then(|mode| mode.split('x').next())
                .and_then(|width| width.parse::<usize>().ok())
                .map(|width| width / 8)
                .filter(|&columns| columns > 0)
                .ok_or_else(|| format!("the GOP line names no mode width: {:?}", lines[query]))?;
            let rows: usize = lines[query + 1..=last].iter().map(|line| line.len().div_ceil(columns).max(1)).sum();

            // A range and not an equality: each panel is dumped after its marker
            // reached the console, so a line drawn in between is on the panel
            // and not in the count.
            let grew = after as i64 - before as i64;
            if !(1..=rows as i64).contains(&grew) {
                return Err(format!(
                    "the panel carried {before} rows at the GOP query and {after} at the loader's \
                     last line, a growth of {grew}, where the loader printed {printed} lines \
                     between them, {rows} rows at {columns} columns\n{console}"
                ));
            }
            eprintln!(
                "  [screen] the panel grew {grew} row(s) across the GOP query, {before} to \
                 {after}, for {printed} line(s) printed"
            );
            Ok(())
        }
        "screen_gop_firmware_mode" => {
            // Four of `GopInfo`'s six fields, on two machines advertising
            // different panels. QMP's `screendump` is the geometry QEMU scans
            // out, the kernel's `GOP:` line is what the bootloader handed it,
            // and `Profile::panel` is what the machine advertises: a loader
            // that picks a mode lands on the largest one both offer. Stride and
            // format ride the same line because neither shows in a geometry — a
            // halved stride shears the picture and a swapped channel order
            // recolours it. **Not reached**: `framebuffer`, `framebuffer_size`.
            let mode_of = |qemu: &mut QemuInstance,
                           label: &str|
             -> Result<(u32, u32, u32, u32), String> {
                let console = qemu.boot_log().to_string();
                // `at 0x` picks the kernel's line; the bootloader's says `fb=`.
                let Some(line) =
                    console.lines().find(|l| l.contains("GOP: ") && l.contains(" at 0x"))
                else {
                    return Err(format!(
                        "{label}: the kernel never logged a GOP line, so it was handed no \
                         framebuffer at all\nboot console:\n{console}"
                    ));
                };
                let field = |key: &str| -> Result<u32, String> {
                    line.split(key)
                        .nth(1)
                        .and_then(|rest| rest.split_whitespace().next())
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| format!("{label}: no `{key}` in {line:?}"))
                };
                let geometry = line
                    .split("GOP: ")
                    .nth(1)
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|g| g.split_once('x'))
                    .ok_or_else(|| format!("{label}: no WxH in {line:?}"))?;
                let kernel = (
                    geometry.0.parse::<u32>().map_err(|e| format!("{label}: {line:?}: {e}"))?,
                    geometry.1.parse::<u32>().map_err(|e| format!("{label}: {line:?}: {e}"))?,
                    field("stride=")?,
                    field("format=")?,
                );
                let dump = qemu.screendump();
                let scanout = (dump.width as u32, dump.height as u32);
                eprintln!(
                    "  [gop] {label}: kernel says {}x{} stride={} format={}, QEMU scans out \
                     {scanout:?}",
                    kernel.0, kernel.1, kernel.2, kernel.3
                );
                if (kernel.0, kernel.1) != scanout {
                    return Err(format!(
                        "{label}: the kernel was handed {:?} and the scanout QEMU is \
                         driving is {scanout:?}",
                        (kernel.0, kernel.1)
                    ));
                }
                Ok(kernel)
            };
            let boot = |profile, _: qemu::LaneFree| {
                QemuInstance::boot_with_options(
                    test_config,
                    c_bins,
                    rust_bins,
                    BootOptions { profile, qmp: true, ..Default::default() },
                )
            };
            let mut qemu = boot(qemu::Profile::Gop, qemu::LaneFree::no_guest_yet());
            let gop = mode_of(&mut qemu, "Gop")?;
            let free = qemu.shutdown();
            let mut qemu = boot(qemu::Profile::Metal, free);
            let metal = mode_of(&mut qemu, "metal-sim")?;

            // The largest mode `-vga std` offers on QEMU's default 16 MiB.
            const LARGEST: (u32, u32) = (2048, 2048);
            // What QEMU's stdvga publishes, measured off these two boots and
            // off a third at 1600x900: it pads no scan line, and its pixels are
            // `PixelBlueGreenRedReserved8BitPerColor`, which `query_gop`
            // encodes as 1.
            const BGR: u32 = 1;
            for (profile, label, mode) in [
                (qemu::Profile::Gop, "Gop", gop),
                (qemu::Profile::Metal, "metal-sim", metal),
            ] {
                let panel = profile.panel().expect("both machines have a VGA adapter");
                if (mode.0, mode.1) != panel {
                    return Err(format!(
                        "{label} advertises a {panel:?} panel and booted into {:?}{} — an \
                         OS inherits the mode its firmware set and does not choose one",
                        (mode.0, mode.1),
                        if (mode.0, mode.1) == LARGEST {
                            ", the largest mode the machine offers"
                        } else {
                            ""
                        }
                    ));
                }
                if mode.2 != panel.0 {
                    return Err(format!(
                        "{label}: the kernel was handed stride={} for a {panel:?} panel this \
                         adapter does not pad — every row it paints would land {} pixels off \
                         the last",
                        mode.2,
                        mode.2 as i64 - panel.0 as i64
                    ));
                }
                if mode.3 != BGR {
                    return Err(format!(
                        "{label}: the kernel was handed format={} for a display that publishes \
                         BGR ({BGR}) — the geometry is right and every colour is wrong",
                        mode.3
                    ));
                }
            }
            // Non-vacuity for the two panel checks above: they compare the
            // guest against a harness constant, so a run in which both machines
            // were handed the same constant would pass them both.
            if gop == metal {
                return Err(format!(
                    "two machines advertising different panels booted into the same {gop:?}"
                ));
            }
            Ok(())
        }
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
                profile: qemu::Profile::Metal,
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
        "screen_early_panel" => {
            // `test-early-halt` stops the boot between `PAT:`'s commit and its
            // repaint, so `PAT:` is on the console and the panel holds only what
            // an earlier record's own repaint put there. That is what tells a
            // repaint per record from one repaint at the end, which would have
            // painted the same tail.
            const LAST: &str = "PAT: IA32_PAT";
            // In order: `arm`, `serial::init`, then `actuator::init` — the first
            // two before `params::init` and the third after it.
            const BEFORE_PARAMS: [&str; 2] =
                ["panic console: armed", "serial: 16550 loopback read"];
            const AFTER_PARAMS: &str = "actuators:";

            let panel = qemu::Profile::Metal.panel().expect("metal-sim advertises a panel");
            let halted_panel = |params: &'static [&'static str]| -> Result<String, String> {
                let mut qemu = QemuInstance::boot_with_options(
                    test_config,
                    c_bins,
                    rust_bins,
                    BootOptions {
                        profile: qemu::Profile::Metal,
                        qmp: true,
                        kernel_params: params,
                        ready_marker: LAST,
                        ..Default::default()
                    },
                );
                // The marker is the record whose repaint never runs, so every
                // paint this boot makes is already on the glass when it lands.
                let dump = qemu.screendump();
                // The machine's panel, not the kernel's account of it: a
                // geometry read off the guest's own `GOP:` line would agree
                // with a guest that painted nothing.
                if (dump.width as u32, dump.height as u32) != panel {
                    return Err(format!(
                        "the machine advertises {panel:?} and the screendump is {}x{}",
                        dump.width, dump.height
                    ));
                }
                let text = dump.text();
                print_screen(name, &text);
                Ok(text)
            };
            let holds = |text: &str, want: &[&str], unwanted: &[&str]| -> Result<(), String> {
                for line in want {
                    if !text.contains(line) {
                        return Err(format!("{line:?} is not on the panel\n{text}"));
                    }
                }
                for line in unwanted {
                    if text.contains(line) {
                        return Err(format!("{line:?} is on the panel\n{text}"));
                    }
                }
                Ok(())
            };

            // Armed: every record up to the halt repaints, so the panel carries
            // the three records before `PAT:` and not `PAT:` itself. `Boot: `
            // and `EARLY PANIC:` are the two other painters, and neither ran.
            let armed = halted_panel(&["test-early-halt", "early-panel"])?;
            holds(
                &armed,
                &[BEFORE_PARAMS[0], BEFORE_PARAMS[1], AFTER_PARAMS],
                &[LAST, "Boot: ", "EARLY PANIC:"],
            )?;

            // The shipping configuration, which names no parameter: the two
            // records before `params::init` repaint because they have no other
            // channel, and nothing after it does.
            let bare = halted_panel(&["test-early-halt"])?;
            holds(
                &bare,
                &BEFORE_PARAMS,
                &[AFTER_PARAMS, LAST, "Boot: ", "EARLY PANIC:"],
            )?;

            eprintln!("  [panel] armed: three records up to the halt, and not {LAST:?}");
            eprintln!("  [panel] no parameter: the two before params::init, and not {AFTER_PARAMS:?}");
            Ok(())
        }
        "screen_log_absent" => {
            // The machine the log partition exists for, with the log partition
            // taken away from it: metal-sim has no serial port a person can
            // read, so a `/log` that did not mount is a fact only the panel can
            // carry. Before this it was carried the way everything else is —
            // one white row, in the middle of phase 5, among sixty-seven — and
            // the owner's report was that nothing said so at all.
            //
            // The diag config for the same reason `screen_diag_boot` uses it:
            // it contains no process that can claim the framebuffer, so the
            // last boot checkpoint's paint is still up when the screendump is
            // taken. On the flashed desktop image the compositor takes the
            // screen about 48 ms after `Boot: complete`, which is what makes
            // "it is on the panel" a claim about the checkpoint and not about
            // how fast a person can look.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("diag");
            let (image_path, _, _) = common::volumes::image_with_unnamed_log_partition(
                "log-absent-boot.img",
                &config,
                &[],
                &[],
            )?;
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                boot_image: Some(qemu::Staged::Written(image_path.clone())),
                ready_marker: "Boot: complete",
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;
            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
            let console = qemu.boot_log().to_string();
            let dump = qemu.screendump_until(common::volumes::NO_LOG_ALERT, Duration::from_secs(30));
            let text = dump.text();
            print_screen(name, &text);

            // Non-vacuity, and it is the half that matters: a boot whose log
            // partition mounted would paint the ordinary line, and a screen
            // asserted on without this would pass on a kernel that always says
            // the alarming thing.
            if !console.contains("log-volume: not mounted") {
                return Err(format!(
                    "the kernel mounted a log volume it was never given, so nothing here is \
                     about a missing /log:\n{console}"
                ));
            }
            if console.contains("logd: this boot's kernel log is") {
                return Err(format!(
                    "logd opened a file anyway — a fallback is what this must not do:\n{console}"
                ));
            }

            if !text.contains(common::volumes::NO_LOG_ALERT) {
                return Err(format!(
                    "the panel of a machine with no /log and no console says nothing about \
                     either\ndecoded screen:\n{text}"
                ));
            }
            // Red, and the rest of the screen white. `text()` throws hue away
            // by construction, so this is the only place the difference between
            // "the line is there" and "the line stands out" exists.
            check_colors(&dump, FILL_BOOT, &[common::volumes::NO_LOG_ALERT], "Boot: complete")?;
            // And it is a boot checkpoint's paint rather than a panic's: the
            // fill above says so, and the machine is still running.
            if !text.contains("Boot: complete") {
                return Err(format!(
                    "the alert is on a screen that never reached the end of the boot\n\
                     decoded screen:\n{text}"
                ));
            }
            let _ = std::fs::remove_file(&image_path);
            let row = dump.row_index(common::volumes::NO_LOG_ALERT).expect("checked above");
            eprintln!("  [log] on the panel, in alert red: {}", dump.rows()[row]);
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
                profile: qemu::Profile::Metal,
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
        "screen_console_clear" => {
            // `clear` is the one command whose entire output is the *absence*
            // of output, which is why nothing else in the suite covers it:
            // every other screen assertion looks for something that should be
            // on the panel, and passes whether or not anything else is up
            // there with it. This one asserts what must *not* be there, and
            // the console is the caller that has to get it right — on the
            // machine it is for there is no scrollbar to drag and no second
            // window to read from.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("console");
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                kernel_features: ACTUATOR_KERNEL,
                ready_marker: "console: ready",
                ..Default::default()
            };
            let mut qemu =
                QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
            let font = screen::ConsoleFont::load();

            // `_rendering`, not the plain wait: on a loaded `smp:2` runner the
            // console paints slowly and the budget-scaled 30s window undercounts
            // a later moment in the run, so a guest still drawing was called
            // wedged (`0 of 2073600 pixels`, the paint never arriving). The
            // console freezes when idle, so a real failure still ends the wait a
            // `GUEST_QUIET` after the deadline.
            let before = qemu.screendump_while_rendering(
                Duration::from_secs(30),
                Duration::from_millis(200),
                |d| d.console_text(&font).contains(CONSOLE_PROMPT),
            );
            let before_text = before.console_text(&font);
            if !before_text.contains(CONSOLE_PROMPT) {
                return Err(format!(
                    "no prompt to clear\ndecoded screen:\n{before_text}"
                ));
            }
            // The premise. Clearing a screen that was already blank asserts
            // nothing, and the seeded kernel log is what fills it.
            let filled = before.console_rows(&font).iter().filter(|r| !r.is_empty()).count();
            if filled < 10 {
                return Err(format!(
                    "only {filled} non-blank rows before `clear`, so there was nothing to \
                     leave behind\ndecoded screen:\n{before_text}"
                ));
            }

            // Draw on the glass behind the console's back, which is the state
            // `clear` exists to get a user out of and the one a damage-tracked
            // console can talk itself out of repairing.
            console_type_line(&mut qemu, &font, "test_rs_test_screen_graffiti")?;
            // Settle on the strip below the last cell row rather than on the
            // whole panel: the console goes on drawing -- the command echoes,
            // the shell reprints its prompt -- so most of the glass is being
            // repainted while this waits, and only the strip no cell covers
            // holds still.
            let margin = |d: &screen::Ppm| d.height % screen::GLYPH_H;
            let margin_is = |d: &screen::Ppm, c: [u8; 3]| {
                let m = margin(d);
                m > 0
                    && d.pixels[(d.height - m) * d.width..].iter().all(|p| *p == c)
            };
            let painted_over = qemu.screendump_while_rendering(
                Duration::from_secs(30),
                Duration::from_millis(200),
                |d| margin_is(d, GRAFFITI),
            );
            // Non-vacuity, in the two places it can be lost. A panel that is a
            // whole number of glyph rows tall has no strip at all, and would
            // make half of what follows assert nothing -- both 2048x2048, the
            // mode this profile used to be given, and `DEFAULT_PANEL`, the one
            // its firmware sets when no panel is declared, are exactly that.
            if margin(&painted_over) == 0 {
                return Err(format!(
                    "this panel is {}x{}, a whole number of {}px glyph rows, so the strip this \
                     test is half about does not exist here",
                    painted_over.width, painted_over.height, screen::GLYPH_H
                ));
            }
            // And if the kernel never reached the glass there is nothing for
            // `clear` to fail to remove.
            let green = painted_over.pixels.iter().filter(|p| **p == GRAFFITI).count();
            if !margin_is(&painted_over, GRAFFITI) || green * 2 < painted_over.pixels.len() {
                return Err(format!(
                    "the graffiti actuator did not reach the panel: {green} of {} pixels are \
                     {GRAFFITI:?} and the {}px strip below the cells is {}",
                    painted_over.pixels.len(),
                    margin(&painted_over),
                    if margin_is(&painted_over, GRAFFITI) { "green" } else { "not" }
                ));
            }

            // Typed onto the paint, and still confirmed by the console's own
            // echo: the shell reprinted its prompt after the graffiti child
            // exited, so the cells it drew are the console's again and the ones
            // it did not draw are still green. That is why the echo is matched
            // as a prefix of the input row (`console_type_line`) — the rest of
            // that row is the actuator's paint and stays.
            console_type_line(&mut qemu, &font, "clear")?;

            // `clear` is `ESC[2J ESC[H`, after which the shell reprints its
            // prompt at the home position. So the whole panel is one row of
            // prompt and nothing else -- wait for that, then assert it, so a
            // slow paint reads as a failure rather than as a pass on a screen
            // that had not finished.
            let only_prompt = |d: &screen::Ppm| {
                let rows = d.console_rows(&font);
                rows.first().is_some_and(|r| r.trim() == CONSOLE_PROMPT)
                    && rows[1..].iter().all(|r| r.is_empty())
            };
            let dump = qemu.screendump_while_rendering(
                Duration::from_secs(30),
                Duration::from_millis(200),
                only_prompt,
            );
            let after = dump.console_text(&font);
            print_screen(name, &after);

            // The pixel assertion first, because it is the specific one: a
            // screen still covered in paint fails the prompt check too, and
            // that message would send the next reader after the shell.
            if let Some(i) = dump.pixels.iter().position(|p| *p == GRAFFITI) {
                let (x, y) = (i % dump.width, i / dump.width);
                let m = dump.height % screen::GLYPH_H;
                let where_ = if y >= dump.height - m {
                    format!("the {m}px strip below the last cell row, which no cell covers")
                } else {
                    format!("cell ({}, {})", x / screen::GLYPH_W, y / screen::GLYPH_H)
                };
                let left = dump.pixels.iter().filter(|p| **p == GRAFFITI).count();
                return Err(format!(
                    "{left} pixels survived `clear`, the first at ({x}, {y}) — {where_}.\n\
                     ESC[2J promises a blank panel; a repaint that skips every cell whose \
                     contents already matched what it believed was there does not deliver one, \
                     and the cells it skips are exactly the ones a user cannot fix any other \
                     way\ndecoded screen:\n{after}"
                ));
            }

            let rows = dump.console_rows(&font);
            if !rows.first().is_some_and(|r| r.trim() == CONSOLE_PROMPT) {
                return Err(format!(
                    "`clear` did not leave the prompt on the home row\n\
                     decoded screen:\n{after}"
                ));
            }
            let survivors: Vec<String> = rows[1..]
                .iter()
                .enumerate()
                .filter(|(_, r)| !r.is_empty())
                .map(|(i, r)| format!("    row {}: {r}", i + 1))
                .collect();
            if !survivors.is_empty() {
                return Err(format!(
                    "{} rows survived `clear`:\n{}\ndecoded screen:\n{after}",
                    survivors.len(),
                    survivors.join("\n")
                ));
            }

            // Not the cell grid but the pixels outside it. A panel whose
            // height is not a whole number of glyph rows has a strip along the
            // bottom that no cell covers, and a console that paints only its
            // cells never writes there -- so whatever drew last, the kernel's
            // last boot checkpoint, stays for the life of the session. Black
            // on black hides it on the machine that found this; a fill that is
            // not black does not.
            eprintln!(
                "  [clear] {}x{}: {} cell rows and a {}px strip below them, none of it left \
                 painted",
                dump.width,
                dump.height,
                dump.height / screen::GLYPH_H,
                dump.height % screen::GLYPH_H
            );
            Ok(())
        }
        "screen_console_scroll" => {
            // The standing check on the emulator's delivery: not "did the
            // right thing appear" but "is the glass exactly what the model
            // says it is", asserted over a workload built to break it.
            //
            // What closed #90 was the owner reporting prior text surviving in
            // the middle of a cleared screen, which means cells the model had
            // written off still held glyphs. `clear` was where he noticed it;
            // this asserts every row of the panel character for character
            // after the scrolling stops, so a single stale glyph fires it at
            // the batch that produced it, with no `clear` needed to expose it.
            //
            // Line lengths vary, past the panel's width as well as under it:
            // the cells a scroll must clear are the ones past the end of a
            // line that replaces a longer one, and a line wider than the panel
            // is the only way one logical line scrolls the screen twice. Batch
            // sizes drift against the row count, and the last round arrives as
            // one block.
            //
            // **The workload is sized by what it must cover, not by a line
            // count.** `test_screen_churn` documents the construction; what
            // this end of it relies on is that any `cols` consecutive lines
            // end in every column of the panel once, and that one line in
            // eight wraps twice — so three rounds walking *disjoint* stretches
            // of 260 lines between them cover both, and a longer run buys the
            // same states again at other alignments. That is not free: the
            // guest recomposes the whole panel for every batch the console
            // reads, measured at 0.21 ms per byte of output under TCG, so the
            // cost of this test is its byte count and nothing else.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("console");
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                kernel_features: ACTUATOR_KERNEL,
                ready_marker: "console: ready",
                ..Default::default()
            };
            let mut qemu =
                QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);
            let font = screen::ConsoleFont::load();

            let before = qemu.screendump_while(
                Duration::from_secs(30),
                Duration::from_millis(200),
                |d| d.console_text(&font).contains(CONSOLE_PROMPT),
            );
            if !before.console_text(&font).contains(CONSOLE_PROMPT) {
                return Err(format!(
                    "no prompt to churn from\ndecoded screen:\n{}",
                    before.console_text(&font)
                ));
            }
            let rows = before.height / screen::GLYPH_H;
            let cols = before.width / screen::GLYPH_W;

            // The same lines `test_screen_churn` prints. Duplicated
            // deliberately: a reference taken from the guest would agree with
            // the guest about a defect they shared.
            let wraps = [0usize, 1, 0, 2, 0, 1, 0, 0];
            let churn_line = |i: usize| -> String {
                let body = 5 + (i * 37) % cols + cols * wraps[i % wraps.len()];
                let fill = char::from(b'a' + (i % 26) as u8);
                let mid: String = std::iter::repeat_n(fill, body).collect();
                format!("L{i:04} {mid} E{i:04}")
            };
            // A logical line wider than the panel occupies more than one row.
            // The emulator wraps when a character arrives at a full row, so a
            // line of exactly `cols` takes one row and not two.
            let display_rows = |line: &str| -> Vec<String> {
                let ch: Vec<char> = line.chars().collect();
                if ch.is_empty() {
                    return vec![String::new()];
                }
                ch.chunks(cols).map(|c| c.iter().collect()).collect()
            };

            // Disjoint stretches tiling one run longer than the panel is wide,
            // so every column of it is the last column of some line. Each
            // round prints more than a panel's worth of rows, so the screen it
            // is asserted on holds nothing from the round before.
            let rounds = [
                (1usize, 0usize, 100usize, 7usize),
                (2, 100, 60, 7),
                (3, 160, 100, 0),
            ];
            assert!(
                rounds.windows(2).all(|w| w[0].1 + w[0].2 == w[1].1)
                    && rounds.iter().map(|r| r.2).sum::<usize>() >= cols,
                "the rounds must tile one run of at least {cols} lines, or some column of \
                 the panel is never the end of a line and the cells past it are never at risk"
            );
            for (round, start, count, chunk) in rounds {
                if round == 2 {
                    // Page back into history and return, mixing the scrollback
                    // view into the same session before more live output. The
                    // view offset changes what every row of the panel means,
                    // and it is the one input the damage pass takes that the
                    // cell grid does not.
                    //
                    // **Two batches, each inside the device queue, each
                    // confirmed on the glass.** A page key is `0xE0`-prefixed,
                    // so a press and its release are four set-1 bytes; three fit
                    // the queue and so do two, and the queue is empty at the
                    // first because the round before ran to `CHURN-DONE`. Both
                    // batches must move the view — the round before printed a
                    // hundred lines of history and the page down is off a
                    // non-zero offset — so the panel changing is the guest
                    // saying it read them.
                    for (keys, batch) in [(3usize, "pgup"), (2, "pgdn")] {
                        let was = qemu.screendump();
                        {
                            let mut input = qemu::QmpInput::open(qemu.qmp_socket());
                            let mut events: Vec<(&str, bool)> = Vec::new();
                            for _ in 0..keys {
                                events.extend([(batch, true), (batch, false)]);
                            }
                            assert!(
                                events.len() * 2 <= QEMU_PS2_QUEUE,
                                "{} transitions of {batch} are up to {} set-1 bytes against a \
                                 {QEMU_PS2_QUEUE}-byte device queue",
                                events.len(),
                                events.len() * 2
                            );
                            input.keys(&events);
                        }
                        let moved = |d: &screen::Ppm| !d.identical_to(&was);
                        let now = qemu.screendump_while_rendering(
                            CONSOLE_ECHO,
                            Duration::from_millis(50),
                            moved,
                        );
                        if !moved(&now) {
                            return Err(format!(
                                "{keys} {batch} presses moved nothing on the panel, so the \
                                 console never read them — QEMU's {QEMU_PS2_QUEUE}-byte PS/2 \
                                 queue drops what a guest that is not draining cannot take, \
                                 silently\ndecoded screen:\n{}",
                                now.console_text(&font)
                            ));
                        }
                    }
                }
                console_type_line(
                    &mut qemu,
                    &font,
                    &format!("test_rs_test_screen_churn {start} {count} {chunk} {cols}"),
                )?;
                // When the round is over is a different question from whether
                // the panel is right, and asking the panel both at once is how
                // a broken panel used to spend the whole timeout and then
                // report that a marker never arrived. The console writes the
                // glass before it mirrors the same bytes to its own stdout, so
                // the marker on the console stream means that batch is painted
                // — whatever it painted. The prompt is not on the stream: the
                // shell writes it without a newline, so nothing line-oriented
                // ever sees it, and the bottom row is what says the child has
                // exited.
                //
                // The wait is the guest's own: a round is a hundred lines of
                // console traffic, so silence is a console that stopped and
                // never a console that is behind. It used to be 45 s of host
                // clock, and `round 1: the guest never printed CHURN-DONE` at
                // 598 s in the wide phase was that number expiring rather than
                // anything about this panel (`issues/build/`).
                let done = format!("CHURN-DONE {start} {count}");
                let mut printed = String::new();
                if let Err(why) = await_guest(
                    &mut qemu,
                    &mut printed,
                    &format!("round {round} to print `{done}`"),
                    |seen| seen.contains(&done),
                ) {
                    return Err(format!("{why}\nround {round} printed:\n{printed}"));
                }
                let settled = |d: &screen::Ppm| {
                    d.console_rows(&font)
                        .last()
                        .is_some_and(|l| l.trim_end().starts_with(CONSOLE_PROMPT))
                };
                let dump =
                    qemu.screendump_while(Duration::from_secs(15), Duration::from_millis(100), settled);
                let decoded = dump.console_rows(&font);
                let text = dump.console_text(&font);
                if !settled(&dump) {
                    return Err(format!(
                        "{STALLED} round {round}: the prompt never came back to the bottom row, \
                         so the panel was still being painted when it was read\ndecoded screen:\n\
                         {text}"
                    ));
                }
                if !decoded.iter().any(|l| l.trim() == done) {
                    return Err(format!(
                        "round {round}: `{done}` never reached the panel\ndecoded screen:\n{text}"
                    ));
                }

                // Expand every line this round printed into the rows it
                // occupies, then take the tail the panel holds. Built from the
                // whole round rather than from a guess at how many lines fit,
                // because a wrapped line makes those different numbers.
                let mut all: Vec<String> = Vec::new();
                for i in start..start + count {
                    all.extend(display_rows(&churn_line(i)));
                }
                all.push(done.clone());
                if all.len() < rows {
                    return Err(format!(
                        "round {round}: {count} lines occupy {} rows, which does not fill a \
                         {rows}-row panel — what is left on it belongs to the round before, \
                         and this round would be asserted against rows it never printed",
                        all.len()
                    ));
                }
                let want: Vec<String> = all[all.len() - (rows - 1)..].to_vec();

                for (r, expect) in want.iter().enumerate() {
                    let got = decoded[r].trim_end();
                    if got == expect.trim_end() {
                        continue;
                    }
                    let col = got
                        .chars()
                        .zip(expect.chars())
                        .position(|(a, b)| a != b)
                        .unwrap_or(expect.chars().count().min(got.chars().count()));
                    let longer = got.chars().count() > expect.trim_end().chars().count();
                    return Err(format!(
                        "round {round}: panel row {r} is not what the console holds.\n\
                         first difference at column {col}{}\n\
                         want: {expect:?}\n\
                         got:  {got:?}\n\
                         The glass disagrees with the model, so a cell was written off as \
                         delivered without being blitted\ndecoded screen:\n{text}",
                        if longer {
                            " — the row on screen is LONGER than the line that belongs there, so \
                             what is past its end is left over from before"
                        } else {
                            ""
                        }
                    ));
                }
                let last = decoded[rows - 1].trim_end();
                if !last.starts_with(CONSOLE_PROMPT) {
                    return Err(format!(
                        "round {round}: the prompt is not on the bottom row, it reads {last:?}\n\
                         decoded screen:\n{text}"
                    ));
                }
                eprintln!(
                    "  [scroll] round {round}: lines {start}..{} at {} per flush, all {} rows \
                     match the model character for character",
                    start + count,
                    if chunk == 0 { count } else { chunk },
                    rows - 1
                );
            }
            Ok(())
        }
        "screen_console_panic" => {
            // Does claiming the framebuffer silence the panic report? Read off
            // the code the answer is no — `render` ignores
            // SCREEN_OWNED_BY_USERLAND entirely and only `boot_checkpoint`
            // honours it — but nothing in the suite had ever staged the state
            // that answers it: `screen_fatal_halt` boots `tests/testcases`,
            // whose init list contains no framebuffer claimer at all, so the
            // flag is false on every screen test that panics.
            //
            // Staged the real way round: the panic is triggered *through the
            // console*, by typing at its prompt, so the screen the report has
            // to paint over is a screen a userland process drew and owns.
            // Unlike `screen_console_shell` this one carries the test binaries
            // and a kernel feature, so it is not the flashed image — what it
            // certifies is the kernel's behaviour, not the artifact.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("console");
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                kernel_features: ACTUATOR_KERNEL,
                ready_marker: "console: ready",
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;
            let mut qemu =
                QemuInstance::boot_with_options(&config, c_bins, rust_bins, options);

            let font = screen::ConsoleFont::load();
            let before = qemu.screendump_while(
                Duration::from_secs(30),
                Duration::from_millis(200),
                |d| d.console_text(&font).contains(CONSOLE_PROMPT),
            );
            // The premise. Without a console-drawn screen underneath, a
            // report reaching the panel proves nothing about ownership and
            // this test would be `screen_fatal_halt` on a different config.
            if !before.console_text(&font).contains(CONSOLE_PROMPT) {
                return Err(format!(
                    "no console prompt to panic over\ndecoded screen:\n{}",
                    before.console_text(&font)
                ));
            }

            // Confirmed keystroke by keystroke, because the two times this test
            // has ever gone red the command never reached the shell: QEMU's
            // PS/2 queue had dropped part of it and the assertion below then
            // reported the panic path for a panic nobody had asked for. See
            // `console_type_line`.
            console_type_line(&mut qemu, &font, "test_rs_test_panic_child 3")?;

            let dump = qemu.screendump_until(FATAL_HALT_NONCE, Duration::from_secs(40));
            let text = dump.text();
            print_screen(name, &text);
            if !text.contains(FATAL_HALT_NONCE) {
                return Err(format!(
                    "the fatal report never took the screen back from the console — which \
                     would make `/system/bin/console` a downgrade on the machine it is for\n\
                     decoded screen (kernel font):\n{text}\n\
                     decoded screen (console font):\n{}",
                    dump.console_text(&font)
                ));
            }
            // The fill is what says the report repainted the *whole* screen
            // rather than landing in a corner of the console's.
            if dump.fill() != FILL_FATAL {
                return Err(format!(
                    "the report is on screen but the fill is {:?}, not the fatal {FILL_FATAL:?}",
                    dump.fill()
                ));
            }
            if dump.console_text(&font).contains(CONSOLE_PROMPT) {
                return Err(format!(
                    "the console's prompt survived the report, so the panic painted over \
                     part of the screen and left the rest\ndecoded screen:\n{text}"
                ));
            }
            eprintln!(
                "  [console] the fatal report took the screen back from a userland owner"
            );
            Ok(())
        }
        "screen_i8042_health" => {
            // The health verdict on the only machine that needs it on glass: no
            // 16550, no virtio-console, so the log ring has nowhere to drain and
            // the panel is the whole diagnostic. Nothing in this image claims
            // DEVICE_FRAMEBUFFER, which is the other half of the condition.
            //
            // Not a panic: `screen_late_panic` covers the fatal path, and what
            // is under test here is a *successful* boot repainting to say
            // something the last boot checkpoint could not have known yet.
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                mute: true,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            metal_sim_argv_check(&argv)?;
            match argv.iter().position(|a| a == "-serial") {
                Some(i) if argv.get(i + 1).is_some_and(|v| v == "none") => {}
                _ => return Err(format!("the muted profile still has a 16550: {argv:?}")),
            }

            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            // The verdict waits for a CPU with nothing left to run, so it lands
            // after the last boot checkpoint by construction. 30s covers
            // firmware plus the root filesystem read off USB.
            let dump = qemu.screendump_until("never asserted", Duration::from_secs(30));
            let text = dump.text();
            print_screen(name, &text);
            if !text.contains("never asserted") {
                return Err(format!(
                    "the i8042 health verdict never reached the panel of a guest with no \
                     console at all\ndecoded screen:\n{text}"
                ));
            }
            // A panic carries the log tail too, and would satisfy the search
            // above while meaning something entirely different.
            if dump.fill() != FILL_BOOT {
                return Err(format!(
                    "screen fill is {:?}, want the boot checkpoint's {FILL_BOOT:?} — this is \
                     a panic report, not a health verdict\ndecoded screen:\n{text}",
                    dump.fill()
                ));
            }
            // The line the verdict follows on from must still be there: a
            // repaint that dropped the boot log would be a worse diagnostic
            // than no repaint.
            if !text.contains("Boot: complete") {
                return Err(format!(
                    "the repaint lost the boot log it was supposed to extend\n\
                     decoded screen:\n{text}"
                ));
            }
            let row = dump.row_index("never asserted").expect("checked above");
            eprintln!("  [i8042] on the panel of a console-less guest: {}", dump.rows()[row]);
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
                profile: qemu::Profile::Metal,
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
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
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
                    profile: qemu::Profile::Virt,
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
                    profile: qemu::Profile::VirtEl2,
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
                    profile: qemu::Profile::Virt,
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
            // The port's stage 4 on one CPU, under the EL2 profile whose
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
                    profile: qemu::Profile::VirtEl2,
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
            virt_job("preempt", "preempt: the counting thread was preempted twice")
        }
        "virt_fp_isolation" => virt_job("fp_isolation", "fp_isolation: v0-v31, FPCR and FPSR survived"),
        "virt_first_entry" => virt_job("first_entry", "first_entry: x1-x30 were zero"),
        "virt_unmap_touch" => virt_job("unmap_touch", "unmap_touch: 4 reads of a page just unmapped"),
        "virt_debug_refused" => virt_job(
            "debug_refused",
            "debug_refused: SYS_DEBUG's double fault and TLB acknowledgement delay were refused",
        ),
        "virt_readonly_copyout" => {
            virt_job(&format!("test_rs_{VIRT_COPYOUT}"), "a syscall writes only where its caller could store")
        }
        "virt_irq_storm" => {
            // The CPU floods itself with SGIs until the timer has fired a
            // thousand times through the flood, then waits for every SGI it
            // sent. A tick lost or never re-armed, or an SGI lost, leaves the
            // storm running and the verdict unsaid.
            virt_selftest(test_config, &["irq-storm"])
        }
        "virt_timer_floor" => virt_selftest(test_config, &["timer-floor"]),
        "screen_late_panic" => {
            // The ordinary fatal panic, which no userland process can produce:
            // crash_report, capture, panic_flush, halt_all_cpus, render. The
            // flush drains the ring before the paint, so the snapshot capture()
            // took is the only thing left to paint from.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Gop,
                    qmp: true,
                    kernel_params: &["test-late-panic"],
                    ready_marker: "PANIC:",
                    ..Default::default()
                },
            );
            // Here the marker reaches serial *before* the paint — the drain is
            // what emits it — so unlike the halt paths this one has to look
            // more than once. And once the report outgrows one screen the
            // pager cycles it, so the window in which any given page is up is
            // `PAGE_HOLD_NS`, not forever: the timeout has to cover a whole
            // cycle rather than just the paint.
            let dump = qemu.screendump_until("PANIC:", Duration::from_secs(30));
            let text = dump.text();
            print_screen(name, &text);
            for want in ["PANIC:", "test-late-panic: on-screen console check"] {
                if !text.contains(want) {
                    return Err(format!("{want:?} not on screen\ndecoded screen:\n{text}"));
                }
            }
            check_colors(
                &dump,
                FILL_FATAL,
                &["PANIC:", "test-late-panic: on-screen console check"],
                "late_panic::Nest",
            )?;
            check_wrap(&dump)?;
            // Written after `capture()` and before the paint. On the console it is proof
            // the record exists; off the panel it is proof the panel painted the
            // snapshot, since a no-op `capture()` leaves `render()` re-reading the ring.
            const AFTER_CAPTURE: &str = "test-late-panic: after the capture";
            let said = qemu.console_stream().since(0);
            if !said.contains(AFTER_CAPTURE) {
                return Err(format!(
                    "{AFTER_CAPTURE:?} never reached the console, so its absence from the \
                     panel says nothing:\n{said}"
                ));
            }
            if text.contains(AFTER_CAPTURE) {
                return Err(format!(
                    "{AFTER_CAPTURE:?} is on the panel — the report was re-read from the \
                     record ring at paint time, not painted from the snapshot `capture()` \
                     froze\ndecoded screen:\n{text}"
                ));
            }
            Ok(())
        }
        "screen_paged_scrollback" => {
            // The screen is smaller than the report, and on the target laptop
            // there is no key to press for the rest of it. So the claim under
            // test is not "the console renders" — `screen_late_panic` has that
            // — but "a line the report page cannot hold reaches the screen
            // anyway, with no input". Same feature and image as
            // `screen_late_panic`, so it costs a boot and no rebuild.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Gop,
                    qmp: true,
                    kernel_params: &["test-late-panic"],
                    ready_marker: "PANIC:",
                    ..Default::default()
                },
            );

            // The first kernel line of the boot, and the one a photograph of
            // the final screen has never been able to show.
            const HEAD: &str = "panic console: armed";
            const TAIL: &str = "PANIC:";

            let mut pages: Vec<String> = Vec::new();
            let mut report: Option<String> = None;
            let mut head_seen = false;
            // **The only incremental paints a guest makes**: the report's own
            // paint follows a fill, and every page the pager puts up after it
            // is written against the grid the one before left — which is the
            // paint `check_no_stale_cells` exists for. The footers of the
            // settled captures it judged, and two of them, because the first is
            // the page the fill painted.
            const JUDGED_PAGES: usize = 2;
            let mut judged: Vec<String> = Vec::new();
            let mut before: Option<String> = None;
            // A liveness ceiling on a machine that is halted and paging, so
            // there is no console to read progress off and this is the case
            // `qemu::budget` exists for.
            let deadline = Instant::now() + qemu.budget(Duration::from_secs(40));
            while Instant::now() < deadline
                && !(head_seen && report.is_some() && judged.len() >= JUDGED_PAGES)
            {
                let dump = qemu.screendump();
                let text = dump.text();
                let Some(footer) = text.lines().rev().find(|l| l.starts_with("[page ")) else {
                    // Before the panic the screen still carries a boot
                    // checkpoint; only a paginated screen has a footer.
                    before = None;
                    thread::sleep(Duration::from_millis(200));
                    continue;
                };
                if !pages.contains(&footer.to_string()) {
                    pages.push(footer.to_string());
                }
                if text.contains(TAIL) {
                    report = Some(text.clone());
                }
                head_seen |= text.contains(HEAD);
                // **A screendump is not a shutter**: one taken across a paint
                // carries the rows already written above the rows the paint
                // replaced, and a row half of each is in no line of any log. Two
                // identical captures are a paint that finished.
                if before.as_deref() == Some(text.as_str()) {
                    check_no_stale_cells(&dump, &qemu.console_stream().since(0))?;
                    if !judged.contains(&footer.to_string()) {
                        judged.push(footer.to_string());
                    }
                }
                before = Some(text.clone());
                thread::sleep(Duration::from_millis(200));
            }

            let seen = pages.join(" ");
            print_screen(name, &format!("footers seen: {seen}"));
            let Some(report) = report else {
                return Err(format!(
                    "{STALLED} {TAIL:?} never reached the screen; footers seen: {seen}"
                ));
            };
            // The premise. If one screen holds both ends there is nothing to
            // page and the rest of this test would pass vacuously — which is
            // the shape the metal-track review kept finding.
            if report.contains(HEAD) {
                return Err(format!(
                    "one screen holds both {HEAD:?} and {TAIL:?}; nothing to page\n{report}"
                ));
            }
            if !head_seen {
                return Err(format!(
                    "{HEAD:?} never reached the screen — the pager did not advance past the \
                     report. footers seen: {seen}\nreport page:\n{report}"
                ));
            }
            if pages.len() < 2 {
                return Err(format!(
                    "only one page footer ever appeared ({seen}); the pager is not cycling"
                ));
            }
            if judged.len() < JUDGED_PAGES {
                return Err(format!(
                    "only {} settled page(s) were judged for stale cells ({}), so no paint made \
                     against the grid the one before it left was ever read",
                    judged.len(),
                    judged.join(" ")
                ));
            }
            Ok(())
        }
        "screen_pager_keys" => {
            // The halted pager takes PageUp off the i8042 with every
            // CPU stopped, and this is the only place that claim can be made:
            // the decode is `toyos-ps2`'s and host-tested, but that a keystroke
            // reaches a machine which has stopped scheduling is a fact about
            // the controller and the poll, not about the table.
            //
            // `Profile::Metal` because QEMU routes injected keys to one handler
            // per device class: every profile with a `usb-kbd` sends them there
            // instead, and this is the only GOP machine without one.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    qmp: true,
                    kernel_params: &["test-late-panic"],
                    ready_marker: "PANIC:",
                    ..Default::default()
                },
            );
            let socket = qemu.qmp_socket().to_path_buf();

            // The footer only exists once the report overflows the screen, so
            // waiting for one is waiting for the pager to be the thing on
            // screen. `page_forever` returns without looping below two pages.
            // Retried, because a dump taken while the pager is repainting
            // catches a half-written bottom row and no footer at all.
            let footer = |q: &mut QemuInstance| {
                for _ in 0..4 {
                    let text = q.screendump().text();
                    if let Some(f) = text.lines().rev().find(|l| l.starts_with("[page ")) {
                        return Some(f.to_string());
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                None
            };
            let deadline = Instant::now() + qemu.budget(Duration::from_secs(30));
            let mut last = loop {
                if let Some(f) = footer(&mut qemu) {
                    break f;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "{STALLED} no `[page n/m]` footer ever appeared; nothing was paging"
                    ));
                }
            };

            // The unattended deadline moves the page on its own, which is
            // waited for before a key is pressed because the first key retires
            // it for good.
            loop {
                let Some(now) = footer(&mut qemu) else {
                    return Err(format!(
                        "{STALLED} the footer vanished while waiting for the unattended deadline"
                    ));
                };
                if now != last {
                    last = now;
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "{STALLED} the pager did not advance on its own — nothing here can say \
                         whether a keystroke stops it"
                    ));
                }
            }

            // **One keystroke, then its page, then the next keystroke**, and
            // every key is PageUp: the unattended deadline only ever moves the
            // page forward, so a page one back is the key's and nothing else's.
            // The first key races the deadline it retires, so its page may come
            // after one more forward move; from the second on, every move has to
            // be exactly one page back, and a forward one is the deadline still
            // running under a reader who has taken the wheel. No clock of the
            // host's is in any of it: a guest that is slow costs this run wall
            // clock and never a move.
            const SAMPLES: usize = 30;
            let page_of = |footer: &str| -> Option<(usize, usize)> {
                let (n, m) = footer.strip_prefix("[page ")?.split_once(']')?.0.split_once('/')?;
                Some((n.trim().parse().ok()?, m.trim().parse().ok()?))
            };
            for key in 1..=SAMPLES {
                qemu::qmp_send_keys(&socket, &[("pgup", true), ("pgup", false)]);
                let by = Instant::now() + qemu.budget(Duration::from_secs(20));
                let now = loop {
                    let Some(now) = footer(&mut qemu) else {
                        return Err(format!(
                            "{STALLED} the footer vanished after {} of {SAMPLES} keystrokes",
                            key - 1
                        ));
                    };
                    if now != last {
                        break now;
                    }
                    if Instant::now() >= by {
                        return Err(format!(
                            "{STALLED} keystroke {key} of {SAMPLES} left the pager on {last:?}: a \
                             PageUp reached a halted machine and no page came of it"
                        ));
                    }
                };
                let ((was, pages), (is, _)) = page_of(&last)
                    .zip(page_of(&now))
                    .ok_or_else(|| format!("unreadable footers {last:?} and {now:?}"))?;
                let back = if was == 1 { pages } else { was - 1 };
                if key > 1 && is != back {
                    return Err(format!(
                        "keystroke {key} of {SAMPLES} moved the page from {last:?} to {now:?}, \
                         not one back — the deadline is still running under a reader who has \
                         taken the wheel, which is what it must not do"
                    ));
                }
                last = now;
            }
            print_screen(name, &format!("every one of {SAMPLES} PageUps moved the page one back"));
            Ok(())
        }
        "screen_fatal_halt" => {
            // The steady-state fatal path: userland is up, the display is
            // idle, and SYS_DEBUG action 3 runs halt_all_cpus for real.
            //
            // The path this covers used to paint a *single line*: nothing had
            // panicked during boot, so the idle loop had drained the ring into
            // the console long before, and `capture` found only what was
            // logged since the last drain. It is the case that proves the ring
            // retains what serial has already collected.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Gop,
                    qmp: true,
                    kernel_features: ACTUATOR_KERNEL,
                    ..Default::default()
                },
            );
            if !qemu.command_until(
                "run test_rs_test_panic_child 3",
                FATAL_HALT_NONCE,
                Duration::from_secs(15),
            ) {
                return Err(format!("{FATAL_HALT_NONCE:?} never reached the console"));
            }
            // Polled, not sampled once: the report is longer than a screen
            // here, so the nonce is on one page of a cycling set.
            let dump = qemu.screendump_until(FATAL_HALT_NONCE, Duration::from_secs(30));
            let text = dump.text();
            print_screen(name, &text);
            if !text.contains(FATAL_HALT_NONCE) {
                return Err(format!(
                    "{FATAL_HALT_NONCE:?} reached serial but not the screen\ndecoded screen:\n{text}"
                ));
            }
            // The teeth for ring *retention*, and the only ones in the suite:
            // this is the one screen test whose panic comes after the
            // scheduler exists, so it is the only one where the idle loop has
            // already drained the log to serial. Reading the drained cursor
            // instead of the retained window painted exactly one row here —
            // the nonce, and no context at all — which every assertion above
            // passes happily, because the nonce *was* that row.
            //
            // Counted rather than matched on a particular line: which line
            // lands on the page carrying the nonce depends on how much
            // userland printed, and the measured states are 1 row and 96, so
            // any bound between them is a five-fold margin rather than a
            // threshold anyone has to tune.
            const MIN_CONTEXT_ROWS: usize = 20;
            let filled = dump.rows().iter().filter(|r| !r.is_empty()).count();
            if filled < MIN_CONTEXT_ROWS {
                return Err(format!(
                    "the fatal report is {filled} rows: the ring kept only what serial had not \
                     taken\ndecoded screen:\n{text}"
                ));
            }
            if dump.fill() != FILL_FATAL {
                return Err(format!("fatal fill is {:?}, want {FILL_FATAL:?}", dump.fill()));
            }
            Ok(())
        }
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
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Gop,
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
                profile: qemu::Profile::Metal,
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
        "screen_blocked_dump" => {
            // Ctrl+Alt+D on the machine it exists for: metal-sim with the
            // 16550 taken away, a compositor holding the screen, and therefore
            // no channel out of the guest at all except the panel. The report
            // has to take the screen back — declining because userland owns it,
            // which is what a boot checkpoint does, would answer the owner's
            // question into a log file nothing is left running to flush.
            //
            // The verdict is asserted on the *panel* and nowhere else, and it
            // is the summary rather than any one thread: the summary is what
            // tells the three states apart, and a photograph that has it has
            // the answer.
            //
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/desktopaudiocase");
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                smp: 8,
                qmp: true,
                mute: true,
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;
            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
            // The compositor's wallpaper first. Every kernel paint fills the
            // panel with `FILL_BOOT`, so a fill that is anything else is
            // userland holding the screen and nothing else — which is the
            // precondition, and asserting on the fill rather than on the
            // absence of boot text is what stops a merely-blank screen passing
            // for a desktop.
            let up = qemu.screendump_while(Duration::from_secs(30), Duration::from_millis(200), |d| {
                d.fill() != FILL_BOOT
            });
            if up.fill() == FILL_BOOT {
                return Err(
                    "the compositor never took the screen, so this would have tested a \
                     checkpoint rather than a report that seizes the panel"
                        .to_string(),
                );
            }

            // Retried, like every other typed handshake in this file: a
            // keystroke that lands while a desktop is still settling reaches a
            // machine that repaints over the answer, and the retry is cheaper
            // than a rule about when a desktop is finished.
            //
            // Polled on the whole verdict rather than on one string of it. A
            // screendump is not a shutter: QEMU converts the panel while the
            // guest is still drawing on it, so a capture taken across a paint
            // carries the rows already drawn and nobody's rows for the rest.
            // A predicate satisfied by `== VERDICT:` alone accepts one of those
            // and then asserts on the missing half — which is what this test
            // did, and what made it intermittent on a quiet host.
            //
            // **A count of keystrokes rather than a span of host seconds.** It
            // was `budget(40 s)` outside a `budget(4 s)` poll, which is ten
            // tries at every width and reads as forty seconds — the number the
            // reader of a red then goes looking for. Ten is the number.
            const DUMP_TRIES: usize = 10;
            let mut dump = up;
            for _ in 0..DUMP_TRIES {
                if report_is_photographable(&dump, "").is_ok() {
                    break;
                }
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
                dump = qemu.screendump_while(
                    Duration::from_secs(4),
                    Duration::from_millis(100),
                    |d| report_is_photographable(d, "").is_ok(),
                );
            }
            let text = dump.text();
            print_screen(name, &text);
            report_is_photographable(&dump, "the report the keystroke painted")?;

            let row = dump.row_index("== VERDICT:").expect("checked above");
            eprintln!(
                "  [dump] on the panel of a guest with no console: {}",
                dump.rows()[row].trim()
            );
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

/// The boot a run of adjacent machine tests shares.
struct Boot {
    group: &'static str,
    qemu: QemuInstance,
    /// What the group has collected off the console since the ready marker.
    ///
    /// **A console is a stream and `drain_serial` consumes it.** The first
    /// member to wait for a line the compositor prints once takes that line
    /// away from every later member — which cost `metal_sim_window_caps` the
    /// `compositor: at most` line the first time these four shared a boot. So
    /// the group holds the console and the members that read boot-time lines
    /// read *this*, which carries the same text each of them got when it owned
    /// the boot. It is not everything the guest ever said: a member wanting a
    /// window that starts empty still drains for itself.
    console: String,
}

/// The boot a run of adjacent machine tests shares, if one is up.
type Grouped = Option<Boot>;

const METAL_SIM_DESKTOP: &str = "metal-sim desktop";
const I8042_TRACE: &str = "i8042 trace";
const LOCALE_WIZARD: &str = "locale wizard";
const SSHD_LOGIN: &str = "sshd login";

/// The line `tests/toyos-rust-tests/src/bin/i8042_keyboard.rs` prints once it
/// holds the keyboard claim, and the line every injection into that binary is
/// timed off. Its callers wait for it, and one — `i8042_undecoded_bytes` —
/// also reads its capture *from* it: it is the boundary between what the
/// machine did on its own and what this test staged.
const I8042_READY: &str = "===I8042_READY===";

/// The shared boot this machine test runs on, or `None` if it owns its own.
///
/// **Two conditions decide membership and neither is cost.** No member may kill
/// the guest, because the rest of the group is queued behind it; and no member
/// may leave state a later one reads. `readdir_bound` is the standing
/// counter-example — it fills `/tmp` to the VFS listing limit and would refuse
/// every later `read_dir` in that guest — and it is why the answer has to be
/// obviously no rather than probably. Where a member does write something the
/// compositor holds, the group's order is the argument: the observer runs
/// against an untouched desktop and the window cap runs before anything else
/// has taken a window.
///
/// Adjacency in [`MACHINE_TESTS`] is what makes a group one boot rather than
/// two: a non-member between two members takes the guest down, because only one
/// may exist at a time (see [`run_machine_test`]).
fn group_of(name: &str) -> Option<&'static str> {
    match name {
        "metal_sim_compositor"
        | "metal_sim_scanout_wc"
        | "metal_sim_window_caps"
        | "metal_sim_ipc_hostile_peer"
        | "metal_sim_compositor_stall"
        | "metal_sim_client_death" => Some(METAL_SIM_DESKTOP),
        "i8042_no_spurious_wake" | "i8042_mouse" => Some(I8042_TRACE),
        // The positive wizard first: it applies a layout, and the negative
        // member reads only its own window, so the order is the argument that
        // no member reads state another left.
        "locale_detect" | "locale_detect_unrecognized" => Some(LOCALE_WIZARD),
        // One guest with a key in its image and a forward into its port 22:
        // the exec arms clean up after themselves, the file arm writes names
        // nothing else looks at, and the auth arm reads only its own console
        // lines.
        "sshd_exec" | "sshd_files" | "sshd_key_auth" => Some(SSHD_LOGIN),
        _ => None,
    }
}

/// The machine every member of `group` runs on, booted by the first member to
/// ask for it.
fn group_boot<'a>(
    held: &'a mut Grouped,
    group: &'static str,
    boot: impl FnOnce() -> QemuInstance,
) -> &'a mut Boot {
    if held.is_none() {
        let qemu = boot();
        let console = qemu.boot_log().to_string();
        *held = Some(Boot { group, qemu, console });
    }
    let up = held.as_mut().expect("just booted");
    assert_eq!(up.group, group, "run_machine_test releases a boot before another group asks");
    up
}

/// `tests/metalcase` on [`qemu::Profile::Metal`]: the T14's device shape with a
/// compositor on the firmware framebuffer, carrying the client binaries its
/// members run.
///
/// Those and not the whole rust set — metalcase's ROOT is four programs and
/// the rest would add tens of megabytes to a boot that needs these.
fn boot_metal_sim_desktop(rust_bins: &[(String, Vec<u8>)]) -> QemuInstance {
    const CLIENTS: [&str; 4] =
        ["window_caps", "ipc_hostile_peer", "compositor_stall", "compositor_client_death"];
    let missing: Vec<&str> = CLIENTS
        .iter()
        .copied()
        .filter(|want| !rust_bins.iter().any(|(name, _)| name == want))
        .collect();
    assert!(missing.is_empty(), "the metal-sim clients were not built: {missing:?}");
    let bins: Vec<(String, Vec<u8>)> = rust_bins
        .iter()
        .filter(|(name, _)| CLIENTS.contains(&name.as_str()))
        .cloned()
        .collect();

    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/metalcase");
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        ..Default::default()
    };
    metal_sim_argv_check(&qemu::profile_argv(&options)).unwrap_or_else(|e| panic!("{e}"));
    QemuInstance::boot_with_options(&config, &[], &bins, options)
}

/// Metal-sim with the i8042 driver's per-drain trace on and QMP open, which is
/// how a test injects a key or a pointer packet and then reads what the driver
/// made of it.
fn boot_i8042_trace(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> QemuInstance {
    // On metal-sim, because that is the machine the driver is for and the
    // absent USB HID is what makes these tests measure anything: QEMU routes
    // injected input to one handler per device class, and with a usb-kbd
    // present that handler is not the PS/2 one.
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        kernel_params: &["i8042-trace"],
        ..Default::default()
    };
    metal_sim_argv_check(&qemu::profile_argv(&options)).unwrap_or_else(|e| panic!("{e}"));
    QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options)
}

/// The machine the layout and wizard tests run on.
///
/// `Profile::Metal` for the same reason `boot_i8042_trace` uses it: QEMU
/// activates one input handler per device class, so with a USB HID present the
/// injected keys would not reach the i8042 — and these tests are about which
/// HID usage a physical key position reports. `tests/testcases` boots neither
/// the compositor nor `/system/bin/console`, so the keyboard claim is free for
/// `locale_gate` to take — which it does, because it is standing in for a
/// surface and a surface holds the keyboard.
fn boot_locale(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> QemuInstance {
    let options =
        BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() };
    metal_sim_argv_check(&qemu::profile_argv(&options)).unwrap_or_else(|e| panic!("{e}"));
    QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options)
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

/// One `key=value` out of a `compositor: frames=…` line.
///
/// Every compositor gate reads this line, so they read it the same way: a key
/// that is not there is a compositor whose instrument changed shape, which is
/// a different failure from a number that is too large and says so.
fn compositor_field(stats: &str, key: &str) -> Result<u64, String> {
    let raw = stats
        .split_whitespace()
        .find_map(|f| f.strip_prefix(key))
        .ok_or_else(|| format!("no {key} in the compositor's stats line: {stats}"))?;
    raw.parse::<u64>().map_err(|_| format!("{key}{raw} is not a number: {stats}"))
}

/// How many pixels the compositor said it was given, off its own startup line.
///
/// Read rather than assumed: every damage gate is a fraction of the screen, and
/// a fraction of a number the harness hardcoded would keep agreeing with itself
/// on a machine whose panel is a different size.
fn compositor_screen_px(console: &str) -> Result<u64, String> {
    let (w, h) = compositor_screen_size(console)?;
    Ok(w as u64 * h as u64)
}

/// Which processes survive the T14's device shape, in their own words.
///
/// The compositor claims a firmware framebuffer and says what it got; netd finds
/// no NIC and exits rather than panic; soundd finds no audio device and stays up
/// on a null sink rather than exiting (hardware absence is a routing state — a
/// no-device machine still serves audio clients, discarding what they play); and
/// sshd, which has no device of its own, finds no netd to bind through and says
/// so instead of dumping a tokio backtrace across the boot. The earlier version
/// read the bottom pixel row instead, which says nothing about any of them and
/// stayed green with their graceful behavior reverted.
///
/// **All four are init's children and nothing supervises them**, so the message
/// is the entire diagnostic and its absence is the whole defect — which is why
/// each is asserted by its own text rather than by anything surviving.
///
/// First in its group, and that is the assertion talking: `cursor == frames`
/// and the stats line are read off a desktop no client has connected to yet.
fn metal_sim_compositor(boot: &mut Boot) -> Result<(), String> {
    // init spawns all four programs without waiting, so test-runner's
    // ready marker races the daemons' own lines. Keep draining until
    // every line has been said or the window closes.
    const WANT: [&str; 4] = [
        "compositor: ready",
        "soundd: no audio device, presenting a null sink",
        "netd: no NIC on this machine, exiting",
        "sshd: no network on this machine, exiting",
    ];
    let stalled = await_guest(&mut boot.qemu, &mut boot.console, "every daemon's own line", |c| {
        WANT.iter().all(|w| c.contains(w))
    })
    .err();
    for want in WANT {
        if !boot.console.contains(want) {
            return Err(format!(
                "{}{want:?} never reached the console:\n{}",
                stalled.map(|why| format!("{why}\n")).unwrap_or_default(),
                boot.console
            ));
        }
    }
    // The compositor's periodic self-measurement, which is how the
    // T14 reports what compositing cost it once it is off the serial
    // port and the log is only a file on the stick. It is emitted from
    // a composited frame, so its absence is a compositor that stopped
    // drawing as much as an instrument that never ran.
    //
    // Three of them, not one: the first covers the boot, which repaints the
    // whole screen, and what the idle gate below is about is every interval
    // after that.
    let intervals = |c: &str| {
        c.lines().filter(|l| l.contains("compositor: frames=") && l.contains("windows=")).count()
    };
    let stalled = await_guest(&mut boot.qemu, &mut boot.console, "three frame batches", |c| {
        intervals(c) >= 3
    })
    .err();
    if let Some(why) = stalled {
        return Err(format!(
            "{why}\nthe compositor reported {} of the three frame batches this reads:\n{}",
            intervals(&boot.console),
            boot.console
        ));
    }
    let console = &boot.console;
    // The compositor reports the mode it was handed, which is the
    // proof it claimed a real firmware framebuffer rather than
    // starting on nothing.
    let Some(mode) = console
        .lines()
        .find_map(|l| l.split("compositor: wallpaper ").nth(1))
    else {
        return Err(format!(
            "the compositor never said what framebuffer it got:\n{console}"
        ));
    };
    let Some(stats) = console.lines().find(|l| l.contains("compositor: frames=")) else {
        return Err(format!(
            "the compositor never reported a composited frame:\n{console}"
        ));
    };
    let frames = compositor_field(stats, "frames=")?;
    let min_us = compositor_field(stats, "composite_us_min=")?;
    let max_us = compositor_field(stats, "composite_us_max=")?;
    let total_us = compositor_field(stats, "composite_us_total=")?;
    let cursor = compositor_field(stats, "cursor=")?;
    // Read for their presence and their shape; what they measure is the cost
    // of moving bytes to a panel, which QEMU's host-RAM framebuffer cannot
    // show. There is deliberately no scanout *read* figure: the compositor
    // holds the mapping as a `window::Screen`, which returns no pixel and
    // hands out no pointer, so a counter for it could only ever be zero.
    compositor_field(stats, "scanout_wr_bytes=")?;
    compositor_field(stats, "scanout_blits=")?;
    compositor_field(stats, "back_rd_bytes=")?;
    compositor_field(stats, "rects=")?;
    compositor_field(stats, "damage_px=")?;
    compositor_field(stats, "windows=")?;
    if frames == 0 || total_us == 0 {
        return Err(format!("the compositor reported a dead instrument: {stats}"));
    }
    if min_us > max_us || max_us > total_us {
        return Err(format!("min/max/total do not order: {stats}"));
    }
    // GOP hands out no hardware cursor (`flags: 0`), so the compositor draws
    // one itself — into the back buffer, and only into frames whose damage
    // reaches it. The first frame repaints the whole screen, so it does. That
    // it is *not* every frame is the point: a cursor nobody moved does not
    // need repainting, and drawing it per frame is what a compositor that
    // composed straight onto the panel had to do.
    if cursor == 0 || cursor > frames {
        return Err(format!(
            "{frames} frames on a shape with no hardware cursor drew {cursor} cursors: \
             {stats}"
        ));
    }

    // What one second of an idle desktop costs. Nothing is on this screen but
    // the wallpaper and the taskbar, and the only thing that changes is the
    // clock — so the largest frame in a settled interval is the readout's own
    // box and nothing else.
    //
    // One percent of the screen is the line because the two shapes it
    // separates are far apart: the readout box is 0.46% of a 1920x1080 panel,
    // the whole taskbar strip is 2.96%, and a full repaint is 100%. The
    // taskbar redrawing whole once a second is what the owner saw flicker.
    let screen_px = compositor_screen_px(console)?;
    let settled: Vec<&str> = console
        .lines()
        .filter(|l| l.contains("compositor: frames="))
        .skip(1)
        .collect();
    let Some(idle) = settled.last() else {
        return Err(format!(
            "the compositor reported one interval and no more, so nothing here saw a settled \
             desktop:\n{console}"
        ));
    };
    let windows = compositor_field(idle, "windows=")?;
    if windows != 0 {
        return Err(format!(
            "this desktop was supposed to have no windows on it, and has {windows}: {idle}"
        ));
    }
    let biggest = compositor_field(idle, "damage_px_max=")?;
    if biggest * 100 > screen_px {
        return Err(format!(
            "an idle desktop's largest frame repainted {biggest} of {screen_px} pixels — over a \
             percent of the screen for a clock tick: {idle}"
        ));
    }
    // And nothing panicked on the way. A daemon mishandling its
    // absent device fails the positive check above; this catches the
    // rest of the boot dying instead.
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    eprintln!("  [metal-sim] compositor up on {}", mode.trim());
    eprintln!("  [metal-sim] {}", stats.trim());
    eprintln!(
        "  [metal-sim] idle: {biggest} px is the biggest frame of {screen_px} on screen — {}",
        idle.trim()
    );
    eprintln!("  [metal-sim] soundd on a null sink, netd exited — both handled their absent device");
    Ok(())
}

/// The scanout's memory type, from the MSR to the mapping the compositor
/// writes through.
///
/// **The speed this exists for is not measurable here and no line below tries
/// to be.** QEMU's framebuffer is host RAM, where a store costs the same under
/// every memory type; what a guest can be held to is the *decision*, and it has
/// three parts that fail independently. `IA32_PAT` must hold WC in the entry
/// the page tables select, which is per-CPU MSR state no page table records.
/// The kernel must combine that entry with the MTRR it read and reach WC — SDM
/// Vol. 3A Table 11-7 gives WC for a WC PAT entry under every MTRR type, so a
/// UC range register has no veto and a boot reporting UC here is one where the
/// entry never landed. And the process holding the scanout must have been given
/// the same type the kernel gave itself, which is the part that decides what a
/// frame costs: the compositor writes through its own page tables.
fn metal_sim_scanout_wc(boot: &mut Boot) -> Result<(), String> {
    const PAT: &str = "PAT: IA32_PAT=";
    const SCANOUT: &str = "GOP: scanout memory type ";
    const MAPPED: &str = "mapped WriteCombining into pid ";

    let _ = await_guest(&mut boot.qemu, &mut boot.console, "the three memory-type lines", |c| {
        [PAT, SCANOUT, MAPPED].iter().all(|w| c.contains(w))
    });
    scanout_wc(&boot.console)
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

/// Pixels one relative pointer count is worth, on a screen of this size.
///
/// `kernel/src/mouse.rs` scales a count into the square 0..32767 space by
/// `REL_SCALE * short / axis`, and the compositor maps that space back by the
/// axis — so the axis cancels and a count is `REL_SCALE * short / 32768` px on
/// both, which is the whole reason the scaling is per-axis. Duplicated here
/// because a test cannot link the kernel, and *checked* rather than trusted:
/// the calibration press in [`metal_sim_window_drag`] is where the cursor
/// actually is, and it fails by name if this arithmetic put it somewhere else.
fn px_per_count(screen_w: u32, screen_h: u32) -> f64 {
    const REL_SCALE: f64 = 64.0;
    REL_SCALE * screen_w.min(screen_h) as f64 / 32768.0
}

/// A window dragged across the desktop by its title bar, and what that cost.
///
/// The owner's report was that moving a window redraws everything. Two things
/// made it true and both are visible from here: the press that starts a drag
/// marked the whole screen dirty, and every damaged pixel was written to the
/// panel more than once because the desktop was composed *onto* the panel. The
/// gate is the compositor's own `damage_px_max`, which is the largest single
/// frame of an interval — the frame the press produced, if the press is still
/// repainting the screen.
///
/// Nothing here aims at the title bar from constants. The client reports the
/// content-local name of every pixel the host presses, so the window's origin
/// is measured, and the same press repeated after the drag is what proves the
/// window moved rather than that the injection was ignored.
fn metal_sim_window_drag(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let bins: Vec<(String, Vec<u8>)> =
        rust_bins.iter().filter(|(name, _)| name == "window_drag").cloned().collect();
    if bins.is_empty() {
        return Err("the window_drag client was not built".to_string());
    }

    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/metalcase");
    let options =
        BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() };
    metal_sim_argv_check(&qemu::profile_argv(&options))?;
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);

    // The compositor announces its screen after the ready marker, so this
    // waits for the line rather than reading a boot log that cannot have it.
    //
    // And it waits for the first *stats* line too, which is the one carrying
    // the frame that painted the desktop for the first time: a gate about what
    // a drag costs must not be handed the boot's own full-screen repaint.
    let mut boot_log = qemu.boot_log().to_string();
    await_marker(&mut qemu, &mut boot_log, "compositor: frames=", "the boot's own repaint interval")
        .map_err(|why| format!("{why}\n{boot_log}"))?;
    let screen_px = compositor_screen_px(&boot_log)?;
    let (screen_w, screen_h) = compositor_screen_size(&boot_log)?;
    let ppc = px_per_count(screen_w, screen_h);

    // Where the host presses, twice: the middle of the screen, which is where
    // the compositor centres a window it was given a size for.
    let probe_x = screen_w / 2;
    let probe_y = screen_h / 2;
    // How far the drag carries the window. Big enough that a rounded count is
    // not most of it, small enough that the pressed pixel is still inside the
    // content afterwards, which is what the second press reads.
    const DRAG_DX: u32 = 120;
    const DRAG_DY: u32 = 60;

    let result = qemu.run_test_hooked(
        "test_rs_window_drag",
        Duration::from_secs(120),
        "===DRAG_READY===",
        |socket| {
            let mut input = qemu::QmpInput::open(socket);
            let counts = |px: f64| (px / ppc).round() as i32;
            // A packet nobody could produce by hand teleports the cursor, and
            // the compositor damages where it was and where it went — so an
            // injection that moves it a screen at a time makes a frame this
            // gate then reads as a defect. Every step here is a plausible
            // flick of a real mouse.
            const STEP_PX: f64 = 120.0;
            let travel = |input: &mut qemu::QmpInput, dx: i32, dy: i32| {
                let step = counts(STEP_PX).max(1);
                let steps = (dx.abs().max(dy.abs()) + step - 1) / step;
                for i in 0..steps.max(1) {
                    let from_x = i * dx / steps.max(1);
                    let to_x = (i + 1) * dx / steps.max(1);
                    let from_y = i * dy / steps.max(1);
                    let to_y = (i + 1) * dy / steps.max(1);
                    input.mouse(to_x - from_x, to_y - from_y, None);
                    thread::sleep(Duration::from_millis(25));
                }
            };
            // Everything is relative to a pointer at the origin, and the
            // kernel clamps its accumulator there, so driving into the corner
            // is a way to know where it is without being told. One screen is
            // the distance: the cursor is on it, so that reaches both edges.
            let home = |input: &mut qemu::QmpInput| {
                travel(input, -counts(screen_w as f64), -counts(screen_h as f64));
            };
            let click = |input: &mut qemu::QmpInput| {
                input.mouse(0, 0, Some(("left", true)));
                thread::sleep(Duration::from_millis(60));
                input.mouse(0, 0, Some(("left", false)));
                thread::sleep(Duration::from_millis(60));
            };

            // One: name the pixel under the middle of the screen.
            home(&mut input);
            travel(&mut input, counts(probe_x as f64), counts(probe_y as f64));
            click(&mut input);

            // Two: up onto the title bar and drag. The window is centred and
            // its content is `CLIENT_H` tall, so the middle of the screen is
            // `CLIENT_H/2` below the content's top edge give or take the few
            // pixels by which the taskbar and the title bar differ — and a
            // little further up is the strip a person grabs to move a window.
            // If this lands in the content instead, the client reports a third
            // press and the assertions below say so by name.
            travel(&mut input, 0, -counts(CLIENT_H as f64 / 2.0 + TITLE_PROBE_PX));
            input.mouse(0, 0, Some(("left", true)));
            thread::sleep(Duration::from_millis(60));
            travel(&mut input, counts(DRAG_DX as f64), counts(DRAG_DY as f64));
            input.mouse(0, 0, Some(("left", false)));
            thread::sleep(Duration::from_millis(120));

            // Three: name the same screen pixel again. It is a different
            // pixel of the window now, by exactly what the drag carried.
            home(&mut input);
            travel(&mut input, counts(probe_x as f64), counts(probe_y as f64));
            click(&mut input);
        },
    );

    if result.error.is_some() || result.exit_code != Some(0) {
        // **`{:?}` on the verdict is what this arm used to say**, and a `Debug`
        // of a multi-line report is one line of `\n` escapes — the kernel's own
        // account of a death, rendered unreadable by a format specifier. It is
        // printed as itself now.
        let why = match &result.error {
            Some(err) => err.to_string(),
            None => String::from("it finished and its exit code is the finding"),
        };
        return Err(format!(
            "window_drag exited {:?}: {why}\n{}",
            result.exit_code, result.stdout
        ));
    }

    // The client ends on the host's second press, so the interval the drag is
    // in is still open when it exits. Waiting for the line that closes it keeps
    // a slower guest a longer run rather than a different verdict.
    let mut text = result.serial;
    text.push_str(
        &qemu.drain_until(Duration::from_secs(10), |l| l.contains("compositor: frames=")),
    );
    if !text.contains(&format!("drag probe: {CLIENT_W}x{CLIENT_H} window up")) {
        return Err(format!(
            "the client did not report a {CLIENT_W}x{CLIENT_H} window, so the aim below is for a \
             window that is not there:\n{text}"
        ));
    }
    let presses: Vec<(i64, i64)> = text
        .lines()
        .filter_map(|l| l.split("drag probe: press at ").nth(1))
        .filter_map(|rest| rest.trim().split_once(','))
        .filter_map(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
        .collect();
    if presses.len() != 2 {
        return Err(format!(
            "the client was pressed inside its content {} times, not twice — the injected \
             pointer never reached it:\n{text}",
            presses.len()
        ));
    }
    let (before, after) = (presses[0], presses[1]);
    // The window moved, so the screen pixel the host pressed is now nearer the
    // window's top-left corner by what the drag carried.
    let moved_x = before.0 - after.0;
    let moved_y = before.1 - after.1;
    let slack = 8;
    if (moved_x - DRAG_DX as i64).abs() > slack || (moved_y - DRAG_DY as i64).abs() > slack {
        return Err(format!(
            "the drag was supposed to carry the window {DRAG_DX},{DRAG_DY} px and carried it \
             {moved_x},{moved_y} — the press missed the title bar, or the drag was not followed:\
             \n{text}"
        ));
    }

    // A fifth of the screen. The window is 400x160 with its chrome, so a drag
    // of it damages the place it left and the place it arrived — well under a
    // tenth of a 1920x1080 panel. A press that still marks the screen dirty is
    // 100%, which is what this separates.
    let mut biggest = 0;
    let mut lines = 0;
    for line in text.lines().filter(|l| l.contains("compositor: frames=")) {
        lines += 1;
        biggest = biggest.max(compositor_field(line, "damage_px_max=")?);
    }
    if lines == 0 {
        return Err(format!("the compositor reported no interval during the drag:\n{text}"));
    }
    if biggest * 5 > screen_px {
        return Err(format!(
            "dragging a {CLIENT_W}x{CLIENT_H} window repainted {biggest} of {screen_px} pixels in \
             one frame — over a fifth of the screen:\n{text}"
        ));
    }

    eprintln!(
        "  [metal-sim] drag carried the window {moved_x},{moved_y} px; biggest frame {biggest} \
         of {screen_px} px over {lines} intervals"
    );
    Ok(())
}

/// The guest runs the cases and asks for a paste after each copy; this half
/// types GUI+V at every ask and asserts what the guest cannot see. **The
/// kernel's record is the independent half**: a compositor that maps the pipe
/// is ended by the kernel with a handle fault, whatever the compositor believed
/// it was doing, so no such record may appear — and each refusal the guest
/// caused must be named by the compositor.
fn metal_sim_hostile_clipboard(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    /// The guest's `PASTE_MARKER`.
    const PASTE: &str = "===HOSTILE_CLIPBOARD_PASTE===";
    let bins: Vec<(String, Vec<u8>)> = rust_bins
        .iter()
        .filter(|(name, _)| name == "compositor_hostile_clipboard")
        .cloned()
        .collect();
    if bins.is_empty() {
        return Err("the compositor_hostile_clipboard client was not built".to_string());
    }
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/metalcase");
    let options =
        BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() };
    metal_sim_argv_check(&qemu::profile_argv(&options))?;
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);

    let result = qemu.run_test_paced(
        "test_rs_compositor_hostile_clipboard",
        Duration::from_secs(240),
        |socket, line| {
            if line.contains(PASTE) {
                let socket = socket.expect("this boot was made with QMP");
                qemu::QmpInput::open(socket).keys(&[
                    ("meta_l", true),
                    ("v", true),
                    ("v", false),
                    ("meta_l", false),
                ]);
            }
        },
    );
    let text = format!("{}\n{}", result.stdout, result.serial);

    for death in ["handle fault:", "exit: compositor"] {
        if let Some(line) = text.lines().find(|l| l.contains(death)) {
            return Err(format!("the kernel ended the compositor: {}\n{text}", line.trim()));
        }
    }
    if result.error.is_some() || result.exit_code != Some(0) {
        let why = match &result.error {
            Some(err) => err.to_string(),
            None => String::from("it finished and its exit code is the finding"),
        };
        return Err(format!(
            "compositor_hostile_clipboard exited {:?}: {why}\n{text}",
            result.exit_code
        ));
    }
    if !text.contains("hostile clipboard: every case survived") {
        return Err(format!("the guest did not report that every case survived:\n{text}"));
    }
    if !text.contains("it began a copy and never committed it") {
        return Err(format!(
            "the client that held its region and never committed was not dropped by name:\n{text}"
        ));
    }
    // The compositor's `DropReason::Retired`, which no other case produces.
    const RETIRED: &str = "it sent the retired clipboard region";
    if !text.lines().any(|l| l.contains("compositor: dropping client") && l.contains(RETIRED)) {
        return Err(format!(
            "the client that sent a pipe where a region went was not refused by name:\n{text}"
        ));
    }
    const NOT_UTF8: [&str; 2] = ["compositor: refusing a clipboard from client", "it is not UTF-8"];
    if !text.lines().any(|l| NOT_UTF8.iter().all(|s| l.contains(s))) {
        return Err(format!("the copy that is not UTF-8 was not refused by name:\n{text}"));
    }
    serial::Serial::named("boot console", result.serial.as_str()).must_be_clean()?;
    eprintln!("  [metal-sim] a pipe refused unused");
    Ok(())
}

/// How far above its content the host reaches for a window's title bar.
///
/// Not the compositor's title-bar height — this is a probe, and what it needs
/// is to land inside a strip whose size it does not know. Twelve pixels is
/// above any border and inside any title bar a person could grab, and either
/// kind of miss is caught by name: too little and the client reports a third
/// press, too much and the window never moves.
const TITLE_PROBE_PX: f64 = 12.0;

/// The window `window_drag` asks for, which is how the host knows where to
/// press. Asserted against the client's own report rather than assumed.
const CLIENT_W: u32 = 400;
const CLIENT_H: u32 = 160;

/// The screen the compositor said it was given, off its own startup line.
fn compositor_screen_size(console: &str) -> Result<(u32, u32), String> {
    let mode = console
        .lines()
        .find_map(|l| l.split("compositor: wallpaper ").nth(1))
        .and_then(|rest| rest.split("scaling to ").nth(1))
        .ok_or_else(|| format!("the compositor never said what screen it got:\n{console}"))?;
    let (w, h) = mode
        .trim()
        .split_once('x')
        .ok_or_else(|| format!("unreadable screen size {mode:?}"))?;
    let w: u32 = w.trim().parse().map_err(|_| format!("unreadable width in {mode:?}"))?;
    let h: u32 = h.trim().parse().map_err(|_| format!("unreadable height in {mode:?}"))?;
    Ok((w, h))
}

/// End's release: the sentinel `test_rs_i8042_keyboard` exits on
/// (`tests/toyos-rust-tests/src/bin/i8042_keyboard.rs`). Every caller that
/// injects through a fresh connection sends this after its last injection.
/// [`i8042_no_spurious_wake`], which holds one connection open for the whole
/// run, sends the same two transitions as the last group of its own script: a
/// `-qmp …,server` socket
/// serves one monitor at a time, so a second one opened here would block.
fn send_i8042_sentinel(socket: &Path) {
    qemu::qmp_send_keys(socket, &[("end", true), ("end", false)]);
}

/// The line `tests/toyos-rust-tests/src/bin/locale_gate.rs` prints in `layout`
/// mode once the surface holds the keyboard and the wizard's child has gone —
/// the moment a key injected at this machine reaches a translator, and the one
/// [`SWISS_SCRIPT`] is started off.
const SWISS_READY: &str = "===SWISS_READY===";

/// What [`swiss_german_layout`] types, as the groups it may have in flight at
/// once, each with the number of `kev` lines the guest owes for it.
///
/// **A group is the unit of pacing, and its size is bounded by
/// [`QEMU_PS2_QUEUE`]**: the widest group here is four transitions, eight set-1 bytes even
/// if every one were `0xE0`-prefixed, against a device holding sixteen. The
/// whole string is far more than the queue, so a host sending it on a wall clock
/// loses its tail to a guest that stops draining.
///
/// One `kev` per transition, releases and modifiers included: the surface
/// reports every event it reads, which makes the count a report of what the
/// guest took off the device rather than of what the host sent.
const SWISS_SCRIPT: &[(&[(&str, bool)], usize)] = &[
    // QWERTZ: the two letters that swap.
    (&[("y", true), ("y", false)], 2),
    (&[("z", true), ("z", false)], 2),
    // The three dedicated umlauts, and the accented vowel Shift gives.
    (&[("bracket_left", true), ("bracket_left", false)], 2),
    (&[("semicolon", true), ("semicolon", false)], 2),
    (&[("apostrophe", true), ("apostrophe", false)], 2),
    (&[("shift", true), ("apostrophe", true), ("apostrophe", false), ("shift", false)], 4),
    // The AltGr layer.
    (&[("alt_r", true), ("2", true), ("2", false), ("alt_r", false)], 4),
    (&[("alt_r", true), ("e", true), ("e", false), ("alt_r", false)], 4),
    (&[("alt_r", true), ("bracket_left", true), ("bracket_left", false), ("alt_r", false)], 4),
    // The ISO key, all three levels the reference gives it a legend for.
    (&[("less", true), ("less", false)], 2),
    (&[("shift", true), ("less", true), ("less", false), ("shift", false)], 4),
    (&[("alt_r", true), ("less", true), ("less", false), ("alt_r", false)], 4),
    // Dead keys: compose, compose with Shift, the capital umlaut this
    // layout has no dedicated key for, the bare form before a space, an
    // AltGr dead key, and one that composes with nothing.
    (&[("equal", true), ("equal", false)], 2),
    (&[("e", true), ("e", false)], 2),
    (&[("equal", true), ("equal", false)], 2),
    (&[("shift", true), ("e", true), ("e", false), ("shift", false)], 4),
    (&[("bracket_right", true), ("bracket_right", false)], 2),
    (&[("shift", true), ("u", true), ("u", false), ("shift", false)], 4),
    (&[("equal", true), ("equal", false)], 2),
    (&[("spc", true), ("spc", false)], 2),
    (&[("alt_r", true), ("minus", true), ("minus", false), ("alt_r", false)], 4),
    (&[("e", true), ("e", false)], 2),
    (&[("equal", true), ("equal", false)], 2),
    (&[("q", true), ("q", false)], 2),
    // And the key the wizard asks about.
    (&[("grave_accent", true), ("grave_accent", false)], 2),
    // The sentinel `test_rs_locale_gate layout` exits on — the same End key
    // and the same reason as [`send_i8042_sentinel`]. Nothing above presses
    // End, so its release is unambiguous, and a run that loses it pays the
    // guest binary's whole fallback instead.
    (&[("end", true), ("end", false)], 2),
];

/// No group of [`SWISS_SCRIPT`] may outrun the device queue even if every
/// transition in it is an `0xE0`-prefixed two-byte one, which is the widest a
/// non-Pause set-1 transition gets.
const _: () = {
    let mut i = 0;
    while i < SWISS_SCRIPT.len() {
        assert!(
            SWISS_SCRIPT[i].0.len() * 2 <= QEMU_PS2_QUEUE,
            "a swiss_german_layout group can outrun QEMU's PS/2 queue, which drops what it \
             cannot hold one byte at a time and says nothing"
        );
        i += 1;
    }
};

/// Swiss German end to end: the real command selects the layout, and the keys
/// a Swiss keyboard has arrive as the characters a Swiss keyboard prints.
///
/// Injection is by *position*: QEMU's qcodes name the US legend of a physical
/// key, so `y` is the key a Swiss board prints `Z` on and `bracket_left` is
/// the one it prints `ü` on. That is exactly the substitution the layout
/// exists to make, so asserting on the characters that come out is asserting
/// on the table, the modifier levels, the ISO key and the dead-key machine at
/// once.
///
/// **Paced against the guest's own report**, for [`SWISS_SCRIPT`]'s reason: a group goes out only once every `kev` line the one
/// before it owed has come back, so at most one group's bytes are ever
/// outstanding at a device that holds sixteen. A guest that stalls costs this
/// test wall clock and never a verdict.
fn swiss_german_layout(qemu: &mut QemuInstance) -> Result<(), String> {
    let sent = std::cell::Cell::new(0usize);
    let seen = std::cell::Cell::new(0usize);
    let result = {
        let mut input: Option<qemu::QmpInput> = None;
        qemu.run_test_paced(
            "test_rs_locale_gate layout",
            Duration::from_secs(30),
            |socket, line| {
                if line.contains(SWISS_READY) {
                    input = Some(qemu::QmpInput::open(
                        socket.expect("swiss_german_layout needs BootOptions { qmp }"),
                    ));
                }
                if line.contains("kev usage=") {
                    seen.set(seen.get() + 1);
                }
                let Some(input) = input.as_mut() else { return };
                // What everything already sent owes. Nothing new goes out until
                // the guest has reported all of it, which is what bounds the
                // bytes outstanding at the device to one group's worth.
                let owed: usize = SWISS_SCRIPT[..sent.get()].iter().map(|(_, n)| n).sum();
                if seen.get() < owed {
                    return;
                }
                if let Some((keys, _)) = SWISS_SCRIPT.get(sent.get()) {
                    input.keys(keys);
                    sent.set(sent.get() + 1);
                }
            },
        )
    };
    let (sent, seen) = (sent.get(), seen.get());
    if let Some(err) = &result.error {
        // The guard, not the verdict: under the pacing the host is *waiting* for
        // the guest when this fires, so what it establishes is that the run
        // stopped and never that the machine dropped a key.
        let owed: usize = SWISS_SCRIPT.iter().map(|(_, n)| n).sum();
        return Err(format!(
            "{STALLED} {err} — {sent} of {} groups sent and {seen} of {owed} key events back \
             when the host gave up waiting for the next\n{}",
            SWISS_SCRIPT.len(),
            result.stdout
        ));
    }
    if !result.stdout.contains("locale: Keyboard layout set to 'swiss-german'") {
        return Err(format!("the real command did not select the layout:\n{}", result.stdout));
    }
    // And the surface was told, and re-read the config rather than being sent
    // a name it had to trust. Without this the assertion below would pass on a
    // gate binary that had simply been built with the layout hard-coded.
    if !result.stdout.contains("surface: layout is now swiss-german") {
        return Err(format!(
            "the surface hosting `locale` never re-read the config it wrote:\n{}",
            result.stdout
        ));
    }

    let events = parse_key_events(&result.stdout);
    if events.is_empty() {
        return Err(format!("no key event reached userland:\n{}", result.stdout));
    }
    let typed: String = events
        .iter()
        .filter(|e| e.modifiers & 0x10 == 0)
        .map(|e| e.translated.as_str())
        .collect();
    // Modifier presses translate to nothing, so the characters are contiguous.
    let want = "zyüöäà@€[<>\\êÊÜ^é^q§";
    if !typed.contains(want) {
        return Err(format!("typed {typed:?}\n  want it to contain {want:?}"));
    }
    // The ISO key really was HID 0x64 and not something the profile faked.
    if !events.iter().any(|e| e.usage == 0x64) {
        return Err(format!("no event for the ISO key in {events:?}"));
    }
    eprintln!("  [swiss-german] {} events, typed {typed:?}", events.len());
    Ok(())
}

/// How many keys the wizard is answered with, at most — `y`, the `§` key, Enter.
const WIZARD_ANSWERS: usize = 3;

/// **The wizard gates are the one injection here a wall clock cannot cost
/// anything**: the answers are fewer bytes than [`QEMU_PS2_QUEUE`] holds and
/// none of their qcodes is `0xE0`-prefixed, so a guest draining nothing for
/// the whole hook still receives every transition. Anything added to the
/// sequence past that bound has to be paced against the guest, the way
/// [`SWISS_SCRIPT`] is.
const _: () = assert!(
    WIZARD_ANSWERS * 2 <= QEMU_PS2_QUEUE,
    "the wizard gates put more at QEMU's PS/2 queue than it holds, and it drops the excess \
     one byte at a time and says nothing — pace them against the guest"
);

/// The wizard, answered as a Swiss keyboard's owner would answer it.
fn locale_detect(qemu: &mut QemuInstance) -> Result<(), String> {
    let result = qemu.run_test_hooked(
        "test_rs_locale_gate detect",
        Duration::from_secs(30),
        "Press the key labelled",
        |socket| {
            let mut input = qemu::QmpInput::open(socket);
            // The key a Swiss board prints `Z` on, then the one it prints `§`
            // on, then Enter to confirm — [`WIZARD_ANSWERS`] of them, which is
            // what the bound beside that constant is about.
            let answers = ["y", "grave_accent", "ret"];
            assert_eq!(answers.len(), WIZARD_ANSWERS, "the wizard's answers outgrew their bound");
            for key in answers {
                input.keys(&[(key, true), (key, false)]);
                thread::sleep(Duration::from_millis(60));
            }
        },
    );
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    for want in [
        "detect: Press the key labelled  Z",
        "detect: Press the key labelled  \u{a7}",
        "detect: That is 'swiss-german'",
        "detect: Keyboard layout set to 'swiss-german'",
        // The wizard held the surface's keys for the whole conversation, and
        // the surface acted on the config it wrote. Both are new: this ran on
        // a machine whose keyboard the gate binary claims, which is the state
        // that used to make the wizard refuse.
        "surface: client",
        "surface: layout is now swiss-german",
    ] {
        if !result.stdout.contains(want) {
            return Err(format!("no {want:?} in:\n{}", result.stdout));
        }
    }
    eprintln!("  [locale detect] identified swiss-german in two presses");
    Ok(())
}

/// The negative control, in the guest: presses no layout agrees with must end
/// in a refusal, never in a verdict.
fn locale_detect_unrecognized(qemu: &mut QemuInstance) -> Result<(), String> {
    let result = qemu.run_test_hooked(
        "test_rs_locale_gate detect",
        Duration::from_secs(30),
        "Press the key labelled",
        |socket| {
            let mut input = qemu::QmpInput::open(socket);
            // `y` is a QWERTZ answer; `d` is where no layout puts `§`. Two,
            // one under [`WIZARD_ANSWERS`]'s bound.
            for key in ["y", "d"] {
                input.keys(&[(key, true), (key, false)]);
                thread::sleep(Duration::from_millis(60));
            }
        },
    );
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    if !result.stdout.contains("detect: Unrecognized") {
        return Err(format!("the wizard did not refuse:\n{}", result.stdout));
    }
    if result.stdout.contains("Keyboard layout set to") {
        return Err(format!("the wizard applied a layout it could not identify:\n{}", result.stdout));
    }
    Ok(())
}

/// A round trip through a guest that is **demonstrably up**, corrected for how
/// fast this host is and not for how many guests it is running.
///
/// [`qemu::budget`] is the ceiling on a guest that might be wedged, and it
/// multiplies by the width because a guest with a twelfth of the machine takes
/// longer over everything. This is the other case, and the width is wrong for
/// it: what these callers wait on is the shell echoing a line it has not run
/// yet, which is microseconds of guest time however little of the machine the
/// guest has. Ten of those establishing nothing is a keystroke path that is not
/// working, and a width-scaled ceiling turns that into four minutes of a lane —
/// measured, on the run this was written from: 285 s of a terminal parked on a
/// pipe it had been parked on since 1.4 s.
fn round_trip(one_guest: Duration) -> Duration {
    let (_, _, num, den) = qemu::host_speed();
    one_guest * num / den
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

/// [`serial_until`] over what arrives *after* `from`.
///
/// For a marker a test asks for more than once. `serial_until` scans the whole
/// capture, so the second ask is answered by the first ask's line and the test
/// carries on against a guest that has not done the thing yet — which is the
/// same defect as reusing a nonce, one layer down.
fn serial_until_new(
    qemu: &mut QemuInstance,
    log: &mut String,
    marker: &str,
    from: usize,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        log.push_str(&qemu.drain_serial(Duration::from_millis(200)));
        if log[from.min(log.len())..].contains(marker) {
            return true;
        }
    }
    false
}

/// Answer the wizard as a Swiss keyboard's owner does — the key that prints
/// `Z`, the key that prints `§`, then Enter — **each key sent only once the
/// wizard has asked for it**.
///
/// The wizard prints a prompt and then blocks on one press, so its own output is
/// the pacing and nothing is ever in flight but the key it is waiting for. The
/// first prompt is the caller's to wait for: what it means is that the surface
/// lent the wizard its keys, and each caller says that in its own words.
fn answer_swiss_wizard(
    qemu: &mut QemuInstance,
    log: &mut String,
    where_: &str,
) -> Result<(), String> {
    for (key, next, doing) in [
        ("y", "Press the key labelled", "the wizard to ask for its second key"),
        ("grave_accent", "That is 'swiss-german'", "the wizard to name a layout"),
    ] {
        let asked = log.len();
        {
            let mut input = qemu::QmpInput::open(qemu.qmp_socket());
            input.keys(&[(key, true), (key, false)]);
        }
        await_marker_new(qemu, log, next, asked, &format!("{doing} {where_}"))
            .map_err(|why| format!("{why}\n{log}"))?;
    }
    let mut input = qemu::QmpInput::open(qemu.qmp_socket());
    input.keys(&[("ret", true), ("ret", false)]);
    Ok(())
}

/// How long one typed line has to come back on the guest's own console.
///
/// What is being waited for is a shell echoing a line it has not run yet, which
/// is a round trip and not work — so this is short, and it is paid only when the
/// line did not arrive. [`shell_type_line`] widens it by the guest's own
/// oversubscription; the two callers that retype at a surface which may not be
/// reading yet scale it per host with [`round_trip`] instead.
const ECHO_TRY: Duration = Duration::from_secs(2);

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

/// How many times [`shell_type_line`] retypes a line the guest did not receive
/// whole before it calls that a defect.
///
/// A loss is in the device queue and leaves the shell having answered a command
/// nobody asked for, so the next attempt starts from a fresh prompt and the
/// retype costs nothing but the attempt. Three, because a channel that loses
/// three lines running is not a busy guest.
const SHELL_TYPE_TRIES: usize = 3;

/// What the guest offers as proof it took the last burst out of the device
/// before the next one goes in.
///
/// **A choice with no default, because the two surfaces cannot answer the same
/// question.** `/system/bin/console` draws each echoed character onto glass this
/// harness decodes; a windowed shell renders into a window the compositor
/// places and mirrors to a line-buffered stdout, so nothing of a line under
/// construction reaches the console at all.
enum Drained {
    /// The decoded input row, for a shell behind `/system/bin/console`.
    Panel(screen::ConsoleFont),
    /// The kernel's drain report, for a shell behind a compositor: the device
    /// path rather than the surface, counting the bytes the queue is measured
    /// in. Needs `i8042-trace`, and [`shell_type_once`] refuses a boot without
    /// it rather than pacing on nothing.
    ///
    /// **A boot that arms it is not the shipping kernel**, and the site that
    /// arms it cannot see that: a non-empty `kernel_params` selects the test
    /// kernel, and `kernel/src/actuator.rs`'s `IMPLIES` adds
    /// `i8042-fast-health` and `i8042-edge-race` — a scheduler pass held inside
    /// the drain path. Two latent inexactnesses: any drain after the mark
    /// counts, and `drain bytes=` counts the aux port too.
    Bytes,
}

/// Set-1 bytes the kernel reports taking off the i8042 in `said`; `bytes=`, not `keys=`.
fn i8042_drained(said: &str) -> usize {
    said.lines()
        .filter_map(|line| line.split("i8042: drain bytes=").nth(1))
        .filter_map(|rest| rest.split_whitespace().next())
        .filter_map(|count| count.parse::<usize>().ok())
        .sum()
}

/// What has gone out on this line so far. `base` is what a previous attempt
/// left in the editor: the next burst lands after it, so the echo begins with both.
struct Sent<'a> {
    mark: usize,
    bytes: usize,
    base: &'a str,
    typed: &'a str,
    burst: &'a str,
}

/// Wait until the guest has taken everything sent so far out of the device, and
/// name the burst it never accounted for if it has not.
fn await_drained(
    qemu: &mut QemuInstance,
    ack: &Drained,
    ceiling: Duration,
    sent: Sent<'_>,
) -> Result<(), String> {
    let Sent { mark, bytes, base, typed, burst } = sent;
    match ack {
        Drained::Panel(font) => {
            let want = format!("{base}{typed}");
            let echoed = |dump: &screen::Ppm| {
                console_input_row(dump, font).is_some_and(|row| row.starts_with(&want))
            };
            let dump =
                qemu.screendump_while_rendering(CONSOLE_ECHO, Duration::from_millis(50), echoed);
            if echoed(&dump) {
                return Ok(());
            }
            Err(format!(
                "the console never echoed the burst {burst:?}: its input line reads {:?}, which \
                 does not begin {want:?}. A keystroke was lost between the host and the shell",
                console_input_row(&dump, font).unwrap_or_default()
            ))
        }
        Drained::Bytes => {
            let deadline = Instant::now() + ceiling;
            loop {
                let drained = i8042_drained(&qemu.console_stream().since(mark));
                if drained >= bytes {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "the kernel took {drained} of the {bytes} set-1 bytes typed so far off \
                         the i8042 inside {ceiling:?}, so the burst {burst:?} went out against a \
                         queue the guest had not emptied"
                    ));
                }
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}

/// Type `line` at a shell this harness cannot read a panel for, press Enter, and
/// **make the guest say what it received before the caller asserts on what it
/// did**.
///
/// **Bounding each batch is not bounding what is in flight.** Every burst waits
/// for [`Drained`] before the next goes out, so each starts against a queue the
/// guest has emptied. A QMP reply proves QEMU's main loop ran and never that a
/// vCPU read port 0x60, which is the only thing that empties it.
///
/// Enter goes out last, behind all of the line's bytes, and the verdict is the
/// shell's echo — which reaches the console only when a newline flushes the
/// surface owner's line-buffered stdout. So a dropped Enter echoes *nothing*
/// rather than something mangled, and the retype lands after the fragment the
/// last attempt left in the editor.
///
/// Read through [`qemu::ConsoleStream`] rather than by draining, because the
/// caller owns the capture: a wait that consumed lines here would take the
/// marker its assertion is waiting for.
fn shell_type_line(qemu: &mut QemuInstance, line: &str, ack: &Drained) -> Result<(), String> {
    let echo = qemu.budget(ECHO_TRY);
    let mut last = String::new();
    for _ in 0..SHELL_TYPE_TRIES {
        match shell_type_once(qemu, line, echo, ack) {
            Ok(()) => return Ok(()),
            Err(said) => last = said,
        }
    }
    Err(format!(
        "{SHELL_TYPE_TRIES} typed lines and the shell echoed none of them whole. A keystroke \
         was lost between the host and the shell — QEMU's {QEMU_PS2_QUEUE}-byte PS/2 queue \
         drops what a guest that is not draining cannot take, silently — so nothing below this \
         would have been asking the guest the question it was written to ask.\nasked for \
         {line:?}; the last attempt was answered with {last:?}"
    ))
}

/// One attempt of [`shell_type_line`]. `Err` carries what the guest said
/// instead, which is the evidence and not a message.
///
/// `echo` is a liveness ceiling and never the pacing: it is paid only when the
/// line did not arrive, and a healthy shell echoes inside a round trip.
fn shell_type_once(
    qemu: &mut QemuInstance,
    line: &str,
    echo: Duration,
    ack: &Drained,
) -> Result<(), String> {
    assert!(
        !line.contains('\n'),
        "shell_type_line presses Enter itself; {line:?} carries its own"
    );
    assert!(
        !matches!(ack, Drained::Bytes) || qemu.i8042_trace_armed(),
        "a windowed shell can acknowledge a burst only through the kernel's drain report, so a \
         boot that paces on Drained::Bytes has to arm `i8042-trace`; this one did not, and \
         {line:?} would have gone out unpaced"
    );
    let mark = qemu.console_stream().mark();
    // The row is trimmed, so an empty editor comes back as the bare prompt and
    // loses the space the console draws after it — put it back, or every first
    // burst is compared against a prefix the panel never shows.
    let base = match ack {
        Drained::Panel(font) => {
            let dump = qemu.screendump();
            match console_input_row(&dump, font) {
                Some(row) if row.len() > CONSOLE_PROMPT.len() => row,
                _ => format!("{CONSOLE_PROMPT} "),
            }
        }
        Drained::Bytes => String::new(),
    };
    let mut typed = String::new();
    let mut bytes = 0usize;
    for burst in ps2_bursts(line) {
        {
            // Opened and dropped around each burst: a `-qmp …,server` socket
            // serves one monitor at a time, and the panel wait below is a
            // screendump, which needs the socket back.
            let mut input = qemu::QmpInput::open(qemu.qmp_socket());
            input.type_burst(&burst);
        }
        typed.push_str(&burst);
        bytes += burst.chars().map(qemu::scancode_bytes).sum::<usize>();
        let sent = Sent { mark, bytes, base: &base, typed: &typed, burst: &burst };
        await_drained(qemu, ack, echo, sent)?;
    }
    {
        let mut input = qemu::QmpInput::open(qemu.qmp_socket());
        input.keys(&[("ret", true), ("ret", false)]);
    }
    let deadline = Instant::now() + echo;
    loop {
        let said = qemu.console_stream().since(mark);
        if said.contains(line) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(said);
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn shell_answers(qemu: &mut QemuInstance, log: &mut String, ack: &Drained) -> Result<(), String> {
    shell_echoes(qemu, log, "surface-up-zqjxk", ack)
}

/// [`shell_answers`] with the nonce named, for a caller that asks more than
/// once.
///
/// The nonce must differ between asks. `serial_until` scans everything
/// captured so far, so a later question with an earlier answer still in the
/// log is answered by that one whatever the shell is doing.
///
/// **Two waits, because the two ways this fails are different questions.** The
/// first is "has the terminal come up", and it used to be answered by retyping
/// against `qemu.budget(20 s)` — a guess at how long a desktop takes to come up
/// on the host of the day. The terminal knows when it is up and now
/// says so, so this asks it and waits on the guest's own liveness. The second is
/// "does a keystroke reach the shell", and it starts from a machine that is
/// demonstrably up — a ceiling on *that* is a claim about the guest.
fn shell_echoes(
    qemu: &mut QemuInstance,
    log: &mut String,
    nonce: &str,
    ack: &Drained,
) -> Result<(), String> {
    // Whichever surface owner this config put a shell behind, printed once its
    // screen exists and the shell's stdin is a pipe it holds. Before that a
    // keystroke lands nowhere and leaves no trace. Both, because `shell_answers`
    // is asked of a terminal under the compositor and of `/system/bin/console` on the
    // raw framebuffer, and the question is the same one.
    const SURFACE_UP: [&str; 2] = ["terminal: ready", "console: ready"];
    // **And the state in which it is never coming.** `/system/bin/terminal` exits when
    // it loses the race with the compositor (`issues/kernel/`), which is a fact
    // the log states outright at 0.6 s — so waiting for a ready marker that
    // cannot arrive is not a slow guest but a defect, and the only thing a
    // ceiling decides there is how many minutes of a lane it costs to say so.
    // Measured on the run this came from: 305 s, against a terminal that had
    // exited before the compositor was ready.
    const SURFACE_GONE: [&str; 2] = ["exit: terminal ", "exit: console "];
    let up = |log: &str| SURFACE_UP.iter().any(|m| log.contains(m));
    let gone = |log: &str| SURFACE_GONE.iter().any(|m| log.contains(m));
    await_guest(qemu, log, "a surface to say it is up", |log| up(log) || gone(log))?;
    if !up(log) {
        return Err(
            "the surface owner exited before it ever said it was ready — /system/bin/terminal races \
             the compositor at boot, `issues/kernel/`"
                .to_string(),
        );
    }

    // Retyping rather than waiting longer: a keystroke injected between two of
    // the terminal's polls is dropped, and a dropped one leaves nothing to wait
    // for.
    //
    // **A count of attempts, not a span of host seconds.** This used to be a
    // flat twenty, which is a fixed number of round trips on the host it was
    // written on and a different number on any other. Ten is the number, and
    // each gets a round trip scaled to this host — see [`round_trip`] for why
    // that and not the phase width.
    const TRIES: usize = 10;
    let mut lost = String::new();
    for _ in 0..TRIES {
        // One attempt, because here a line that does not come back is the
        // loop's ordinary step: the surface is up and the shell may still not
        // be reading, which is what the retype exists for.
        if let Err(said) =
            shell_type_once(qemu, &format!("echo {nonce}"), round_trip(ECHO_TRY), ack)
        {
            lost = said;
            continue;
        }
        if serial_until(qemu, log, nonce, round_trip(Duration::from_secs(2))) {
            return Ok(());
        }
    }
    Err(format!("{TRIES} typed lines and none of them came back\n{lost}"))
}

/// A shell must get its prompt back when a windowed child's window goes.
///
/// The owner opened snake, closed its window with the X button, and never saw
/// a prompt again. Both readings of his log are testable here and the two
/// probes separate them: the first ends the child by *its own* exit, the
/// second by the compositor taking its window away while it is alive —
/// GUI+Q, which is the same `windows.remove` + `MSG_WINDOW_CLOSE` + drop the
/// X button runs and is a keystroke rather than a guess at where the button
/// is.
///
/// The client is a bare `window::Window`, so a reproduction here is about the
/// shell, the terminal and the window protocol, and a clean run narrows the
/// defect to what winit does that this does not.
/// Close the focused window with GUI+Q, retrying until the compositor says a
/// window went.
///
/// **Never blind, and what it waits for is the close itself.** A keystroke
/// injected while the guest is busy is lost, so one attempt is not enough on a
/// loaded host; but a second GUI+Q *after* one worked closes the next window
/// down, which here is the terminal, and that takes the shell and the whole
/// desktop with it. The compositor emits `window closed` from the close, so
/// this waits on the event it caused. Waiting on the `windows=N` count instead
/// is what made this re-send: that count is a sample taken every two seconds,
/// so it answers about an interval rather than about this injection — and the
/// wait was `serial_until`, which scans the whole capture, so the *previous*
/// probe's `windows=1` returned it immediately and the loop hammered GUI+Q at
/// the speed of a QMP round trip.
///
/// The ceiling is the guest's own liveness rather than a phase-scaled clock,
/// and here that cuts both ways: #156 is a *freeze*, so the machine this
/// retries against goes silent, and the wait ends in fifteen seconds instead of
/// spending `qemu.budget(20 s)` — up to four minutes at width 12 — hammering
/// GUI+Q at a desktop that has stopped. `issues/design-debt/` names that
/// cost as a lane this test holds for a quarter of every run, which is what puts
/// whichever desktop is dispatched beside it into a red nobody acts on.
fn close_focused_window(qemu: &mut QemuInstance, log: &mut String, new: usize) -> bool {
    const CLOSED: &str = "compositor: window closed";
    let mut live = qemu::Liveness::new(Duration::from_secs(15), Duration::from_secs(60));
    while !log[new..].contains(CLOSED) && live.working(log) {
        {
            let mut input = qemu::QmpInput::open(qemu.qmp_socket());
            input.keys(&[("meta_l", true), ("q", true), ("q", false), ("meta_l", false)]);
        }
        serial_until_new(qemu, log, CLOSED, new, Duration::from_secs(4));
    }
    log[new..].contains(CLOSED)
}

/// How many times snake is opened and closed. One green round says very little
/// about a report that arrived once.
const SNAKE_ROUNDS: usize = 3;
/// Turns played in the last round, at four keys each, so that round's snake is
/// a program that has been running and drawing rather than one a second old.
const SNAKE_TURNS: usize = 8;

/// What doom's renderer draws over `demo1`'s first `TICS` tics, as
/// `userland/doom/src/frames.rs` hashes it: the frames doom drew before clang
/// built its C, which a compiler that builds doom correctly draws again.
const DOOM_FRAMES: &str = "874685cf6fd3dfa5";

/// Gate: doom draws, frame for frame, what it drew when another compiler built
/// it.
///
/// One verdict and no clock: the hash of each game tic's frame over a timedemo
/// of `demo1`, which replays identically on any machine at any speed, against
/// [`DOOM_FRAMES`]. A miscompile anywhere in the renderer, the game logic or the
/// demo's playback moves it.
fn doom_frames(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = root.join("tests/doommusiccase");
    let mut qemu = QemuInstance::boot_with_options(&config, &[], rust_bins, BootOptions::default());
    let result = qemu.run_test("test_rs_doom_frames", Duration::from_secs(300));
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("doom's frame check did not finish (exit {:?}):\n{}", result.exit_code, result.stdout));
    }
    let line = result
        .stdout
        .lines()
        .find(|line| line.contains("[frame-check] tics="))
        .ok_or_else(|| format!("doom printed no [frame-check] line:\n{}", result.stdout))?;
    let field = |name: &str| {
        line.split_whitespace()
            .find_map(|word| word.strip_prefix(name))
            .ok_or_else(|| format!("no {name} in {line:?}"))
    };
    let hash = field("hash=")?;
    if hash != DOOM_FRAMES {
        return Err(format!(
            "doom drew other frames than the ones recorded: hash {hash}, recorded {DOOM_FRAMES} \
             ({line})"
        ));
    }
    eprintln!("  [doommusiccase] {line}");
    Ok(())
}

fn desktop_window_child(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let bins: Vec<(String, Vec<u8>)> =
        rust_bins.iter().filter(|(name, _)| name == "window_child").cloned().collect();
    if bins.is_empty() {
        return Err("the window_child client was not built".to_string());
    }
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/desktopcase");
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        ready_marker: "compositor: ready",
        // The T14's core count. A desktop's teardown is four processes handing
        // pipes back to each other, and on two cores most of that is ordered
        // by having nowhere else to run.
        smp: 8,
        // `Drained::Bytes`; off the shipping kernel, and implies fast-health
        // and edge-race.
        kernel_params: &["i8042-trace"],
        ..Default::default()
    };
    metal_sim_argv_check(&qemu::profile_argv(&options))?;
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
    let mut log = qemu.boot_log().to_string();
    match window_child_probes(&mut qemu, &mut log) {
        Ok(()) => Ok(()),
        Err(message) => Err(format!("{message}\n{}", freeze_report(&mut qemu, &mut log))),
    }
}

/// The words on iced's counter's two buttons, and the size iced draws body text
/// at (`iced::Settings::default_text_size`).
const COUNTER_LABELS: [&str; 2] = ["Increment", "Decrement"];
const COUNTER_LABEL_PX: f32 = 16.0;

/// How closely the panel has to carry each of [`COUNTER_LABELS`]' glyph
/// patterns, as a normalised cross-correlation of its luminance with the
/// word's coverage.
///
/// A region without the word — a flat button, its edges, a gradient — shares
/// at most a stroke or an edge with the word's nine letters and correlates with
/// a fraction of it; the word itself, rasterised by another renderer at another
/// subpixel offset, correlates with most of it.
const LABEL_MATCH: f64 = 0.6;

/// The presents an iced window makes with nothing happening to it: one, for
/// the redraw a new window is owed, which the redraw iced asks for on the
/// resize that sizes it joins in the same iteration. The counter's state never
/// changes while nothing touches it. A window whose frame events turn into
/// redraws presents once per frame the compositor composes for as long as it
/// is open.
const IDLE_PRESENTS: u64 = 1;

/// Mirrored in `tests/toyos-rust-tests/src/bin/winit_pace.rs`'s `FRAMES`.
const PACE_FRAMES: u64 = 60;

/// The client and the content rectangle the compositor names in its first
/// `window opened client=N content=X,Y WxH, …` line in `log`, the rectangle as
/// `(x, y, width, height)` in panel pixels.
fn opened_window(log: &str) -> Option<(u32, (usize, usize, usize, usize))> {
    let line = log.lines().find(|line| line.contains("compositor: window opened client="))?;
    let rest = line.split("client=").nth(1)?;
    let (client, rest) = rest.split_once(" content=")?;
    let (at, rest) = rest.split_once(' ')?;
    let (x, y) = at.split_once(',')?;
    let (w, rest) = rest.split_once('x')?;
    let h = rest.split(',').next()?;
    let rect = (x.parse().ok()?, y.parse().ok()?, w.parse().ok()?, h.parse().ok()?);
    Some((client.parse().ok()?, rect))
}

/// The `presents=P frames=F` the compositor says when it closes `client`'s
/// window.
fn closed_counts(log: &str, client: u32) -> Option<(u64, u64)> {
    let line = log
        .lines()
        .find(|line| line.contains(&format!("compositor: window closed client={client} by ")))?;
    let presents = line.split("presents=").nth(1)?.split_whitespace().next()?.parse().ok()?;
    let frames = line.split("frames=").nth(1)?.split_whitespace().next()?.parse().ok()?;
    Some((presents, frames))
}

/// Each window the compositor opened in `log`, in order, with what had closed
/// it by the end of `log`. A client's handle is reused once its window is
/// gone, so a close belongs to the latest open window of its client.
fn window_lives(log: &str) -> Vec<(u32, Option<String>)> {
    let mut lives: Vec<(u32, Option<String>)> = Vec::new();
    for line in log.lines() {
        if let Some((client, _)) = opened_window(line) {
            lives.push((client, None));
        } else if let Some(rest) = line.split("compositor: window closed client=").nth(1) {
            let (client, rest) = rest.split_once(" by ").unwrap_or_else(|| panic!("close line: {line}"));
            let client: u32 = client.parse().unwrap_or_else(|_| panic!("close line: {line}"));
            let by = rest.split(", ").next().unwrap_or_else(|| panic!("close line: {line}"));
            if let Some(open) = lives.iter_mut().rev().find(|(c, by)| *c == client && by.is_none()) {
                open.1 = Some(by.to_string());
            }
        }
    }
    lives
}

/// `text` in the system font, Open Sans Regular, at `px`, laid out on the
/// font's own advances and kerning, as coverage from 0 to 1 cropped to its ink:
/// what the panel has to show wherever that text was drawn, whichever renderer
/// drew it. `(width, height, coverage)`.
fn rendered_text(text: &str, px: f32) -> (usize, usize, Vec<f64>) {
    let path = common::compile::repo_root().join("assets/fonts/OpenSans-Regular.ttf");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
        .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
    let line = font.horizontal_line_metrics(px).expect("Open Sans has horizontal metrics");
    let ascent = line.ascent.ceil() as i32;
    let height = (line.ascent - line.descent).ceil() as usize + 2;
    let mut pen = 1.0f32;
    let mut previous = None;
    let mut glyphs = Vec::new();
    for c in text.chars() {
        if let Some(p) = previous {
            pen += font.horizontal_kern(p, c, px).unwrap_or(0.0);
        }
        let (metrics, coverage) = font.rasterize(c, px);
        let x0 = pen.round() as i32 + metrics.xmin;
        let y0 = ascent - metrics.ymin - metrics.height as i32 + 1;
        glyphs.push((x0, y0, metrics.width, metrics.height, coverage));
        pen += metrics.advance_width;
        previous = Some(c);
    }
    let width = pen.ceil() as usize + 2;
    let mut canvas = vec![0.0f64; width * height];
    for (x0, y0, w, h, coverage) in glyphs {
        for row in 0..h {
            for col in 0..w {
                let (x, y) = (x0 + col as i32, y0 + row as i32);
                assert!(
                    x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < height,
                    "{text:?} at {px}px laid a glyph outside its own line"
                );
                let at = y as usize * width + x as usize;
                canvas[at] = canvas[at].max(f64::from(coverage[row * w + col]) / 255.0);
            }
        }
    }
    let inked = |i: usize| canvas[i] > 0.0;
    let cols: Vec<usize> = (0..width).filter(|&x| (0..height).any(|y| inked(y * width + x))).collect();
    let rows: Vec<usize> = (0..height).filter(|&y| (0..width).any(|x| inked(y * width + x))).collect();
    let (x0, x1) = (cols[0], cols[cols.len() - 1]);
    let (y0, y1) = (rows[0], rows[rows.len() - 1]);
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut cropped = Vec::with_capacity(w * h);
    for y in y0..=y1 {
        cropped.extend_from_slice(&canvas[y * width + x0..=y * width + x1]);
    }
    (w, h, cropped)
}

/// The best normalised cross-correlation of `template`'s coverage with the
/// luminance under any placement of it inside `rect` of `dump`, as a
/// magnitude: text is lighter than its button or darker than its background,
/// and either way it is the text. With it, that placement as
/// `(x, y, width, height)` in panel pixels.
///
/// A placement whose pixels barely vary is skipped: it correlates with
/// nothing, and it is most of any window. So is one that overlaps `taken`,
/// another word's place.
fn best_text_match(
    dump: &screen::Ppm,
    (rx, ry, rw, rh): (usize, usize, usize, usize),
    (tw, th, template): &(usize, usize, Vec<f64>),
    taken: Option<(usize, usize, usize, usize)>,
) -> (f64, (usize, usize, usize, usize)) {
    let (tw, th) = (*tw, *th);
    let x1 = (rx + rw).min(dump.width);
    let y1 = (ry + rh).min(dump.height);
    if x1 < rx + tw || y1 < ry + th {
        return (0.0, (rx, ry, tw, th));
    }
    let (w, h) = (x1 - rx, y1 - ry);
    let luminance: Vec<f64> = (ry..y1)
        .flat_map(|y| (rx..x1).map(move |x| (x, y)))
        .map(|(x, y)| {
            let [r, g, b] = dump.pixels[y * dump.width + x];
            0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)
        })
        .collect();
    // Summed-area tables of the luminance and its square, one row and column
    // of zeros ahead, so any placement's mean and variance are four reads.
    let stride = w + 1;
    let mut sum = vec![0.0f64; stride * (h + 1)];
    let mut squares = vec![0.0f64; stride * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            let v = luminance[y * w + x];
            let at = (y + 1) * stride + x + 1;
            sum[at] = v + sum[at - 1] + sum[at - stride] - sum[at - stride - 1];
            squares[at] = v * v + squares[at - 1] + squares[at - stride] - squares[at - stride - 1];
        }
    }
    let area = |table: &[f64], x: usize, y: usize| {
        table[(y + th) * stride + x + tw] - table[y * stride + x + tw] - table[(y + th) * stride + x]
            + table[y * stride + x]
    };
    let n = (tw * th) as f64;
    let mean = template.iter().sum::<f64>() / n;
    let centred: Vec<f64> = template.iter().map(|t| t - mean).collect();
    let template_norm = centred.iter().map(|c| c * c).sum::<f64>().sqrt();
    let mut best = (0.0f64, (rx, ry, tw, th));
    for y in 0..=(h - th) {
        for x in 0..=(w - tw) {
            let s = area(&sum, x, y);
            let variance = area(&squares, x, y) - s * s / n;
            // Two levels of standard deviation: flat but for noise.
            if variance < 4.0 * n {
                continue;
            }
            if let Some((ox, oy, ow, oh)) = taken {
                let (px, py) = (rx + x, ry + y);
                if px < ox + ow && ox < px + tw && py < oy + oh && oy < py + th {
                    continue;
                }
            }
            let mut dot = 0.0;
            for row in 0..th {
                let line = &luminance[(y + row) * w + x..(y + row) * w + x + tw];
                let pattern = &centred[row * tw..(row + 1) * tw];
                dot += line.iter().zip(pattern).map(|(l, c)| l * c).sum::<f64>();
            }
            let score = (dot / (template_norm * variance.sqrt())).abs();
            if score > best.0 {
                best = (score, (rx + x, ry + y, tw, th));
            }
        }
    }
    best
}

/// iced's own counter example, unmodified, built here from `tests/iced-counter`
/// and launched from the desktop's shell under `stats`: its window opens, both
/// its buttons' labels are on the panel in the system font, it presents only
/// what it has to while nothing happens to it, and it leaves with code 0 when
/// the compositor closes the window.
///
/// The labels are judged off the panel, in the rectangle the compositor says
/// it put the window's pixels, against each word as Open Sans draws it,
/// rendered here by a second rasteriser: iced carries no font of its own, and
/// draws with what fontdb finds in `/system/share/fonts`. The presents are the
/// compositor's count, not the app's. `stats` reports the app's CPU time and
/// peak memory after it leaves.
fn toolkit_iced() -> Result<(), String> {
    let app = "test_rs_iced-counter";
    let labels: Vec<(usize, usize, Vec<f64>)> =
        COUNTER_LABELS.iter().map(|word| rendered_text(word, COUNTER_LABEL_PX)).collect();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let bins = qemu::build_toyos_bins(&root.join("tests/iced-counter"));
    let (mut qemu, mut log, launched) = toolkit_launch(&bins, "iced-counter", &format!("stats {app}"))?;
    let log = &mut log;
    // Its exit ends the wait too: an app that dies before it has a window
    // would otherwise hold the lane for the whole budget.
    let exited = format!("exit: {app} pid=");
    let deadline = Instant::now() + qemu.budget(Duration::from_secs(60));
    while !log[launched..].contains("compositor: window opened") {
        if log[launched..].contains(&exited) || Instant::now() >= deadline {
            return Err(format!("{app} never got a window:\n{}", &log[launched..]));
        }
        log.push_str(&qemu.drain_serial(Duration::from_millis(200)));
    }
    let (client, content) = opened_window(&log[launched..])
        .ok_or_else(|| format!("the compositor's window line did not parse:\n{}", &log[launched..]))?;

    let by = qemu.budget(Duration::from_secs(30));
    // Two words, so two places: "Decrement" alone carries most of
    // "Increment"'s ink in its "crement". The word matched best claims its
    // place, and the other is matched only away from it.
    let matches = |dump: &screen::Ppm| -> [(f64, (usize, usize, usize, usize)); 2] {
        let [first, second] = [0, 1].map(|i| best_text_match(dump, content, &labels[i], None));
        if first.0 >= second.0 {
            [first, best_text_match(dump, content, &labels[1], Some(first.1))]
        } else {
            [best_text_match(dump, content, &labels[0], Some(second.1)), second]
        }
    };
    let dump = qemu.screendump_while(by, Duration::from_millis(250), |dump| {
        matches(dump).iter().all(|(m, _)| *m >= LABEL_MATCH)
    });
    let found = matches(&dump);
    for (word, (m, at)) in COUNTER_LABELS.iter().zip(&found) {
        if *m < LABEL_MATCH {
            return Err(format!(
                "{app}'s window at {content:?} carries no {word:?} in Open Sans at \
                 {COUNTER_LABEL_PX}px apart from the other label: its best correlation with the \
                 word is {m:.3} at {at:?}, under {LABEL_MATCH} ({found:.3?}):\n{}",
                &log[launched..]
            ));
        }
    }
    let matched: Vec<f64> = found.iter().map(|(m, _)| *m).collect();

    // The rectangle holds the app's pixels only while its window is open, so
    // the close has to be of that same window, by this keystroke: an app that
    // died after it opened leaves the rectangle to whatever is behind it.
    if log[launched..].contains(&exited) {
        return Err(format!("{app} left before its window was judged:\n{}", &log[launched..]));
    }
    let closing = log.len();
    if !close_focused_window(&mut qemu, log, closing) {
        return Err(format!("GUI+Q never reached the compositor:\n{}", &log[launched..]));
    }
    if !log[closing..].contains(&format!("compositor: window closed client={client} by GUI+Q")) {
        return Err(format!(
            "GUI+Q closed some other window than {app}'s (client {client}), so the text was \
             not its:\n{}",
            &log[launched..]
        ));
    }
    let (presents, frames) = closed_counts(&log[closing..], client)
        .ok_or_else(|| format!("the compositor's close line did not parse:\n{}", &log[closing..]))?;
    if presents > IDLE_PRESENTS {
        return Err(format!(
            "{app} presented {presents} times ({frames} frame events back) with nothing \
             happening to it, over the {IDLE_PRESENTS} a window that draws only when asked \
             makes:\n{}",
            &log[launched..]
        ));
    }
    // `stats` prints the peak last, once the app is gone and waited for.
    let by = qemu.budget(Duration::from_secs(30));
    if !serial_until_new(&mut qemu, log, "peak mem", closing, by) {
        return Err(format!(
            "{app} did not leave when its window was closed:\n{}",
            &log[launched..]
        ));
    }
    let after = &log[closing..];
    let exit = after
        .lines()
        .find(|line| line.contains(&exited))
        .ok_or_else(|| format!("no exit record for {app}:\n{after}"))?;
    if !exit.contains(" code=0 ") {
        return Err(format!("{app} did not exit cleanly: {exit}\n{after}"));
    }
    let cpu = exit
        .split_once("cpu=")
        .map(|(_, cpu)| cpu.trim())
        .ok_or_else(|| format!("{app}'s exit record names no CPU time: {exit}"))?;
    let peak = after
        .lines()
        .find_map(|line| line.split_once("peak mem").map(|(_, peak)| peak.trim()))
        .ok_or_else(|| format!("`stats` reported no peak for {app}:\n{after}"))?;
    eprintln!(
        "  [toolkit] {app}: window at {content:?}, {COUNTER_LABELS:?} matched at {matched:.3?}, \
         {presents} presents and {frames} frames while idle, exit 0, cpu {cpu}, peak mem {peak}"
    );
    Ok(())
}

/// The toolkit desktop with `bin`, one of `built`, carried and `launch` typed
/// at its shell, the launch line's offset in the returned log.
fn toolkit_launch(
    built: &[(String, Vec<u8>)],
    bin: &str,
    launch: &str,
) -> Result<(QemuInstance, String, usize), String> {
    let bins: Vec<(String, Vec<u8>)> =
        built.iter().filter(|(name, _)| name == bin).cloned().collect();
    if bins.is_empty() {
        return Err(format!("the {bin} client was not built"));
    }
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/toolkitcase");
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        ready_marker: "compositor: ready",
        smp: 8,
        // `Drained::Bytes`, the typed line's pacing.
        kernel_params: &["i8042-trace"],
        ..Default::default()
    };
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
    let mut log = qemu.boot_log().to_string();
    let ack = Drained::Bytes;
    shell_answers(&mut qemu, &mut log, &ack)?;
    let launched = log.len();
    shell_type_line(&mut qemu, launch, &ack)?;
    Ok((qemu, log, launched))
}

/// `window::Waiter`'s claim, which every winit loop here rests on: a wake
/// raised on another thread ends a wait that also watches windows, more of
/// them than a new waiter has room for, and more wakes than its pipe holds
/// are one.
///
/// `test_rs_window_wake` is launched from the desktop's shell, so it holds the
/// shell's compositor, and says OK only if every one of its rounds was ended by
/// the wake.
fn toolkit_window_wake(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (mut qemu, mut log, launched) = toolkit_launch(rust_bins, "window_wake", "test_rs_window_wake")?;
    let log = &mut log;
    let mut live = qemu::Liveness::new(Duration::from_secs(30), Duration::from_secs(120));
    while live.working(log) {
        let said = &log[launched..];
        if said.contains("WINDOW-WAKE-OK") {
            for line in said.lines().filter(|l| l.contains("WINDOW-WAKE-OK")) {
                eprintln!("  [toolkit] {}", line.trim());
            }
            return Ok(());
        }
        if said.contains("WINDOW-WAKE-LOST")
            || said.contains("WINDOW-WAKE-REFUSED")
            || said.contains("panicked")
        {
            return Err(format!("a wake did not end the windows' wait:\n{said}"));
        }
        log.push_str(&qemu.drain_serial(Duration::from_millis(200)));
    }
    Err(format!("test_rs_window_wake never finished:\n{}", &log[launched..]))
}

/// The ToyOS winit backend's loop through winit's own API: user events sent
/// from `AboutToWait` and from another thread, windows redrawn and dropped on
/// another thread, a window dropped in the handler that made it, one dropped in
/// a user event, and a closed window its application keeps.
/// `tests/toyos-rust-tests/src/bin/winit_loop.rs` asserts what the app is
/// delivered; this closes the window it asks to have closed, reads its
/// verdict, and asks the compositor whether every window the app dropped was
/// closed by that drop.
fn toolkit_winit_loop(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (mut qemu, mut log, launched) = toolkit_launch(rust_bins, "winit_loop", "test_rs_winit_loop")?;
    let log = &mut log;
    let mut live = qemu::Liveness::new(Duration::from_secs(40), Duration::from_secs(240));
    let mut closed = false;
    while live.working(log) {
        let said = &log[launched..];
        if said.contains("WINIT-LOOP-OK") {
            // Up to the kept window's close: a window the app still held is
            // closed by its exit, which follows that.
            let Some(gui_q) = said.find(" by GUI+Q, ") else {
                return Err(format!("the winit loop's kept window was never closed by GUI+Q:\n{said}"));
            };
            let upto = said[gui_q..].find('\n').map_or(said.len(), |end| gui_q + end);
            let lives = window_lives(&said[..upto]);
            let Some(((_, kept), dropped)) = lives.split_last() else {
                return Err(format!("the compositor opened no window for the winit loop:\n{said}"));
            };
            if kept.as_deref() != Some("GUI+Q") {
                return Err(format!(
                    "GUI+Q closed a window other than the one the winit loop opened last:\n{said}"
                ));
            }
            if let Some(at) = dropped.iter().position(|(_, by)| by.as_deref() != Some("the client itself")) {
                let (client, by) = &dropped[at];
                return Err(format!(
                    "window {at} (client {client}) of the {} the winit loop dropped was not closed \
                     by its drop before the kept one was closed, but by {by:?}:\n{said}",
                    dropped.len()
                ));
            }
            for line in said.lines().filter(|l| l.contains("WINIT-LOOP stage")) {
                eprintln!("  [toolkit] {}", line.trim());
            }
            eprintln!("  [toolkit] each of the {} dropped windows closed at its drop", dropped.len());
            return Ok(());
        }
        if said.contains("WINIT-LOOP-FAIL") || said.contains("panicked") {
            return Err(format!("the winit loop failed:\n{said}"));
        }
        if !closed && said.contains("WINIT-LOOP CLOSE-ME") {
            closed = true;
            let closing = log.len();
            if !close_focused_window(&mut qemu, log, closing) {
                return Err(format!("GUI+Q never reached the compositor:\n{}", &log[launched..]));
            }
            continue;
        }
        log.push_str(&qemu.drain_serial(Duration::from_millis(200)));
    }
    Err(format!("test_rs_winit_loop never finished:\n{}", &log[launched..]))
}

/// Redraw pacing: an application that asks for its next frame from inside
/// `RedrawRequested` is held to the compositor's frame events, as a Wayland
/// client is to its frame callbacks.
///
/// The verdict is the compositor's own count on the close line: every one of
/// [`PACE_FRAMES`] presents arrived, and none outran the frame event of the
/// one before it, so the presents exceed the frame events by at most the one
/// still on its way when the window closed.
fn toolkit_winit_pace(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let (mut qemu, mut log, launched) = toolkit_launch(rust_bins, "winit_pace", "test_rs_winit_pace")?;
    let log = &mut log;
    let drew = format!("WINIT-PACE drew {PACE_FRAMES} frames");
    let mut live = qemu::Liveness::new(Duration::from_secs(30), Duration::from_secs(120));
    while live.working(log) && !log[launched..].contains(&drew) {
        let said = &log[launched..];
        if said.contains("WINIT-PACE-FAIL") || said.contains("panicked") {
            return Err(format!("the animation failed:\n{said}"));
        }
        if said.contains("exit: test_rs_winit_pace pid=") {
            return Err(format!("test_rs_winit_pace left before it finished drawing:\n{said}"));
        }
        log.push_str(&qemu.drain_serial(Duration::from_millis(200)));
    }
    let (client, _) = opened_window(&log[launched..])
        .ok_or_else(|| format!("the animation never said it was done:\n{}", &log[launched..]))?;
    let closing = log.len();
    if !close_focused_window(&mut qemu, log, closing) {
        return Err(format!("GUI+Q never reached the compositor:\n{}", &log[launched..]));
    }
    let (presents, frames) = closed_counts(&log[closing..], client).ok_or_else(|| {
        format!(
            "GUI+Q closed some other window than the animation's (client {client}):\n{}",
            &log[launched..]
        )
    })?;
    if presents != PACE_FRAMES {
        return Err(format!(
            "the compositor counted {presents} presents of the {PACE_FRAMES} the animation \
             drew:\n{}",
            &log[launched..]
        ));
    }
    if presents > frames + 1 {
        return Err(format!(
            "{presents} presents against {frames} frame events: the animation drew ahead of the \
             compositor, unpaced:\n{}",
            &log[launched..]
        ));
    }
    eprintln!("  [toolkit] an animation presented {presents} times against {frames} frame events");
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

fn window_child_probes(qemu: &mut QemuInstance, log: &mut String) -> Result<(), String> {
    let ack = Drained::Bytes;
    if let Err(why) = shell_answers(qemu, log, &ack) {
        return Err(format!(
            "{why}\nnothing typed at the terminal window reached a shell:\n{log}"
        ));
    }

    // A windowed child that leaves on its own. The shell is in `waitpid` and
    // the compositor never touches its connection, so this is the plain case
    // and it has to work before the second probe means anything.
    shell_type_line(qemu, "test_rs_window_child exit", &ack)?;
    let by = qemu.budget(Duration::from_secs(20));
    if !serial_until(qemu, log, "WINDOW-CHILD-GONE", by) {
        return Err(format!("the windowed child never reported leaving:\n{log}"));
    }
    if let Err(why) = shell_echoes(qemu, log, "after-own-exit-zqjxk", &ack) {
        return Err(format!(
            "{why}\na windowed child exited by itself and the shell never answered again:\n{log}"
        ));
    }

    // The owner's case: the process is alive and the compositor takes its
    // window away underneath it.
    let started = log.len();
    shell_type_line(qemu, "test_rs_window_child", &ack)?;
    // Its own marker, not the one the probe above already printed.
    let by = qemu.budget(Duration::from_secs(20));
    if !serial_until_new(
        qemu,
        log,
        "WINDOW-CHILD-UP",
        started,
        by,
    ) {
        return Err(format!("the windowed child never got a window:\n{log}"));
    }
    // GUI+Q closes the focused window, and a window the compositor has just
    // created is the focused one. Re-injected until the compositor says the
    // window went — a keystroke that lands while the guest is busy is lost —
    // and never blind, because a second GUI+Q after one worked would close the
    // terminal's window instead.
    let before = log.len();
    if !close_focused_window(qemu, log, before) {
        return Err(format!(
            "GUI+Q never reached the compositor:\n{}",
            &log[before.min(log.len())..]
        ));
    }
    let by = qemu.budget(Duration::from_secs(20));
    if !serial_until_new(qemu, log, "WINDOW-CHILD-GONE", before, by) {
        return Err(format!(
            "the compositor closed the window and the client did not leave:\n{}",
            &log[before.min(log.len())..]
        ));
    }
    if let Err(why) = shell_echoes(qemu, log, "after-window-closed-zqjxk", &ack) {
        return Err(format!(
            "{why}\nthe compositor closed a child's window and the shell never answered again \
             — this is the owner's snake report, reproduced:\n{log}"
        ));
    }
    // And the program he actually ran. Everything above is a `window::Window`
    // and nothing else; snake is that under winit and softbuffer, which is the
    // only difference left between this test and his session.
    //
    // Three rounds, and the last one is played first: his snake had run 39 s
    // and spent 22.4 s of CPU when he closed it, and a window closed one
    // second after it opened exercises a quieter program than that. One green
    // round would say very little about a report that arrived once.
    for round in 0..SNAKE_ROUNDS {
        shell_type_line(qemu, "snake", &ack)?;
        // snake prints nothing of its own, so the compositor's second window
        // is what says it is up — and a window it has just created is the
        // focused one, which is what GUI+Q then closes.
        let opened = log.len();
        let by = qemu.budget(Duration::from_secs(20));
        if !serial_until_new(qemu, log, "windows=2", opened, by) {
            return Err(format!("snake never got a window in round {round}:\n{log}"));
        }
        if round + 1 == SNAKE_ROUNDS {
            let mut input = qemu::QmpInput::open(qemu.qmp_socket());
            for _ in 0..SNAKE_TURNS {
                for key in ["left", "down", "right", "up"] {
                    input.keys(&[(key, true), (key, false)]);
                    thread::sleep(Duration::from_millis(120));
                }
            }
        }
        let before = log.len();
        if !close_focused_window(qemu, log, before) {
            return Err(format!(
                "GUI+Q never reached the compositor in round {round}:\n{}",
                &log[before.min(log.len())..]
            ));
        }
        let by = qemu.budget(Duration::from_secs(20));
        if !serial_until_new(qemu, log, "exit: snake", before, by) {
            return Err(format!(
                "snake did not leave when its window was closed in round {round}:\n{}",
                &log[before.min(log.len())..]
            ));
        }
        if let Err(why) = shell_echoes(qemu, log, &format!("after-snake-{round}-zqjxk"), &ack) {
            return Err(format!(
                "{why}\nsnake's window was closed, snake left, and the shell never answered \
                 again (round {round}) — the owner's report, reproduced:\n{log}"
            ));
        }
    }

    eprintln!(
        "  [desktop] a windowed child and {SNAKE_ROUNDS} snakes each left both ways and the \
         shell kept its prompt"
    );
    Ok(())
}

/// What a typed character costs the desktop.
///
/// The owner's report, in his words: entering one character into the terminal
/// redraws the entire terminal. It did, and the mechanism was that `MSG_PRESENT`
/// carried no damage — the emulator already blits one cell into the shared
/// buffer, and the compositor, told only that something had changed, repainted
/// the whole window. The terminal here fills most of the screen, so that was
/// nine tenths of the panel per keystroke.
///
/// The gate is the compositor's own `damage_px_max`, the largest single frame
/// of a reporting interval, over the intervals in which the typing happened.
/// The clock's readout is 0.46% of this screen and is in every interval; a
/// typed character is a two-cell span, far below it; a repainted window is 89%.
/// Two percent sits between them by a factor of forty either way.
fn desktop_typing_damage() -> Result<(), String> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/desktopcase");
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        ready_marker: "compositor: ready",
        // `Drained::Bytes`; off the shipping kernel, and implies fast-health
        // and edge-race.
        kernel_params: &["i8042-trace"],
        ..Default::default()
    };
    metal_sim_argv_check(&qemu::profile_argv(&options))?;
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
    let mut log = qemu.boot_log().to_string();
    // No panel row under a compositor; the kernel's drain report is the answer.
    let ack = Drained::Bytes;
    if let Err(why) = shell_answers(&mut qemu, &mut log, &ack) {
        return Err(format!(
            "{why}\nnothing typed at the terminal window reached a shell:\n{log}"
        ));
    }
    let screen_px = compositor_screen_px(&log)?;

    // Let the interval carrying the boot's full-screen repaint and the
    // terminal's first paint close before anything here is measured: the
    // report after the one that follows the shell's answer. Those are real
    // frames and they are not what this is about.
    let from = log.len();
    await_guest(&mut qemu, &mut log, "two more compositor intervals", |c| {
        c[from..].matches("compositor: frames=").count() >= 2
    })
    .map_err(|why| format!("{why}\n{log}"))?;
    let before = log.len();

    // Eight lines, each typed a character at a time — the shell's echo of each
    // keystroke is a present of its own, which is the thing being measured.
    //
    // Eight and not more because the terminal must not scroll while this runs.
    // A scroll changes every cell and is honestly a whole-window repaint, so it
    // would fail this gate for the one reason that is not a defect. The window
    // is 58 text rows on this screen, `shell_answers` leaves under ten of them
    // used, and eight commands echoed and answered are twenty-four.
    const NONCE: &str = "typing-damage-gate";
    // **Guest-paced.** The eight lines used to go in on a 250 ms host cadence
    // and the sixteen appearances were then waited for; the waiting was already
    // right and the typing was not. A keystroke injected faster than the guest
    // drains its keyboard is a keystroke that never damages a cell, so on a
    // contended host this measured whatever fraction survived — 2 of 16 on a
    // four-guest CI runner, and the message it produced named the shortfall
    // rather than the cause. Each line now waits for its own echo before the
    // next goes in, which costs a slow guest wall clock and never the stimulus.
    for line in 0..8u32 {
        shell_type_line(&mut qemu, &format!("echo {NONCE}"), &ack)?;
        // Two: the shell echoes the command as it is typed and again as its
        // output. The same arithmetic the verdict below makes.
        let want = ((line + 1) * 2) as usize;
        let mut live = qemu::Liveness::new(Duration::from_secs(15), Duration::from_secs(60));
        while log[before..].matches(NONCE).count() < want && live.working(&log) {
            let seen = qemu.drain_serial(Duration::from_millis(100));
            log.push_str(&seen);
        }
    }
    // The interval holding the last keystrokes is the one reported after them.
    let last = log.len();
    await_guest(&mut qemu, &mut log, "the interval after the last line", |c| {
        c[last..].contains("compositor: frames=")
    })
    .map_err(|why| format!("{why}\n{log}"))?;

    let typed = &log[before..];
    // Sixteen: the shell echoes the command as it is typed and again as its
    // output, so eight lines are sixteen appearances. Counting the echo alone
    // would pass on a terminal that painted the keystrokes and never ran them.
    let echoes = typed.matches(NONCE).count();
    if echoes < 16 {
        return Err(format!(
            "{echoes} of the sixteen appearances the eight typed lines owe reached the console, \
             so most of what this measures never happened:\n{typed}"
        ));
    }
    let mut biggest = 0;
    let mut intervals = 0;
    for line in typed.lines().filter(|l| l.contains("compositor: frames=")) {
        intervals += 1;
        biggest = biggest.max(compositor_field(line, "damage_px_max=")?);
    }
    if intervals == 0 {
        return Err(format!("the compositor reported no interval while typing:\n{typed}"));
    }
    // A max over intervals: a fragmenting injector can only weaken this, never
    // manufacture a false red — the exposure is a masked regression.
    if biggest * 50 > screen_px {
        return Err(format!(
            "a keystroke's frame repainted {biggest} of {screen_px} pixels — over two percent of \
             the screen for one character:\n{typed}"
        ));
    }
    eprintln!(
        "  [desktop] eight lines typed, {echoes} appearances; biggest frame {biggest} of \
         {screen_px} px over {intervals} intervals"
    );
    Ok(())
}

/// The wizard under `/system/bin/console`, which is the whole of the surface tree on
/// a machine with no compositor — and the image that gets flashed.
///
/// This is one of the two tests that replaced the refusal gate. `/system/bin/console`
/// claims the keyboard for its entire run, which is exactly the state that
/// used to make `locale detect` print "cannot read the keyboard directly" and
/// stop; the wizard now asks the console for the transitions instead. The
/// closing assertion is that the console's *own* translator moved with the
/// config: the key a US board prints `[` on types `ü` afterwards, and nothing
/// but a re-read of the file this wizard wrote can do that.
fn console_locale_detect() -> Result<(), String> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("console");
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        ready_marker: "console: ready",
        ..Default::default()
    };
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
    let mut log = qemu.boot_log().to_string();
    // The panel, because this is the surface that has one.
    let ack = Drained::Panel(screen::ConsoleFont::load());
    if let Err(why) = shell_answers(&mut qemu, &mut log, &ack) {
        return Err(format!("{why}\nnothing typed at /system/bin/console reached a shell:\n{log}"));
    }

    shell_type_line(&mut qemu, "locale detect", &ack)?;
    await_marker(
        &mut qemu,
        &mut log,
        "Press the key labelled",
        "the wizard to ask for a key under /system/bin/console — the console did not lend it \
         the keyboard",
    )
    .map_err(|why| format!("{why}\n{log}"))?;
    answer_swiss_wizard(&mut qemu, &mut log, "under /system/bin/console")?;

    for want in ["That is 'swiss-german'", "Keyboard layout set to 'swiss-german'"] {
        await_marker(&mut qemu, &mut log, want, &format!("{want:?} under /system/bin/console"))
            .map_err(|why| format!("{why}\n{log}"))?;
    }
    // The console acted on the notification. A prefix, not the whole line: the
    // console is shared and not line-atomic, so a kernel line lands inside
    // this one often enough to matter (it did, first time this ran). *Which*
    // layout it re-read is the assertion below, which does not depend on a
    // line surviving intact.
    await_marker(
        &mut qemu,
        &mut log,
        "console: keyboard layout",
        "the console to re-read the config the wizard wrote",
    )
    .map_err(|why| format!("{why}\n{log}"))?;

    // And the layout is in force for what is typed next. `bracket_left` is the
    // key a US board prints `[` on and a Swiss one prints `ü` on, so this is
    // the substitution the whole exercise exists to make, taken through the
    // console's translator and the shell.
    // **Bounded rather than echoed back**, and it is the one line here that can
    // be: `echo `, the key and Enter are fewer set-1 bytes than the device
    // queue holds, and the wizard's own last answer has just been consumed — so
    // a guest that drains nothing from here still receives every one of them.
    // What `bracket_left` produces is the assertion below, which is that key's
    // arrival stated as the thing under test.
    {
        let mut input = qemu::QmpInput::open(qemu.qmp_socket());
        let typed = "echo ";
        let bytes: usize = typed.chars().map(qemu::scancode_bytes).sum();
        assert!(
            bytes + 4 <= QEMU_PS2_QUEUE,
            "{typed:?} plus the ISO key and Enter is more than the {QEMU_PS2_QUEUE}-byte \
             device queue holds"
        );
        input.type_burst(typed);
        input.keys(&[("bracket_left", true), ("bracket_left", false)]);
        input.keys(&[("ret", true), ("ret", false)]);
    }
    await_marker(&mut qemu, &mut log, "\u{fc}", "the `[` key to produce `ü`")
        .map_err(|why| format!(
            "{why}\ntyping the `[` key after the wizard did not produce `ü`, so the console is \
             still translating with the layout it booted with\n{log}"
        ))?;
    eprintln!("  [console] the wizard identified swiss-german and the console adopted it");
    Ok(())
}

/// The wizard under `/system/bin/terminal`, on a desktop.
///
/// The other half of the refusal gate's replacement, and the deepest the
/// surface tree goes: the compositor claims the keyboard and forwards whole
/// transitions to the focused window, `window::Window` holds the terminal's
/// translator, and the terminal lends the transitions to the wizard three
/// processes below it. Every one of those hops is a place the old design had
/// nothing but translated bytes.
fn desktop_locale_detect() -> Result<(), String> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/desktopcase");
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        ready_marker: "compositor: ready",
        // `Drained::Bytes`; off the shipping kernel, and implies fast-health
        // and edge-race.
        kernel_params: &["i8042-trace"],
        ..Default::default()
    };
    metal_sim_argv_check(&qemu::profile_argv(&options))?;
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
    let mut log = qemu.boot_log().to_string();
    // No panel row under a compositor; the kernel's drain report is the answer.
    let ack = Drained::Bytes;
    if let Err(why) = shell_answers(&mut qemu, &mut log, &ack) {
        return Err(format!(
            "{why}\nnothing typed at the terminal window reached a shell:\n{log}"
        ));
    }

    shell_type_line(&mut qemu, "locale detect", &ack)?;
    await_marker(
        &mut qemu,
        &mut log,
        "Press the key labelled",
        "the wizard to ask for a key inside a terminal — the compositor or the terminal \
         did not carry the transitions",
    )
    .map_err(|why| format!("{why}\n{log}"))?;
    answer_swiss_wizard(&mut qemu, &mut log, "inside a terminal")?;

    for want in ["That is 'swiss-german'", "Keyboard layout set to 'swiss-german'"] {
        await_marker(&mut qemu, &mut log, want, &format!("{want:?} inside a terminal"))
            .map_err(|why| format!("{why}\n{log}"))?;
    }

    // The same substitution as the console gate, one surface deeper: the
    // config went up to the compositor and came back down to this window's
    // translator.
    // **Bounded rather than echoed back**, and it is the one line here that can
    // be: `echo `, the key and Enter are fewer set-1 bytes than the device
    // queue holds, and the wizard's own last answer has just been consumed — so
    // a guest that drains nothing from here still receives every one of them.
    // What `bracket_left` produces is the assertion below, which is that key's
    // arrival stated as the thing under test.
    {
        let mut input = qemu::QmpInput::open(qemu.qmp_socket());
        let typed = "echo ";
        let bytes: usize = typed.chars().map(qemu::scancode_bytes).sum();
        assert!(
            bytes + 4 <= QEMU_PS2_QUEUE,
            "{typed:?} plus the ISO key and Enter is more than the {QEMU_PS2_QUEUE}-byte \
             device queue holds"
        );
        input.type_burst(typed);
        input.keys(&[("bracket_left", true), ("bracket_left", false)]);
        input.keys(&[("ret", true), ("ret", false)]);
    }
    await_marker(&mut qemu, &mut log, "\u{fc}", "the `[` key to produce `ü`")
        .map_err(|why| format!(
            "{why}\ntyping the `[` key after the wizard did not produce `ü`, so the \
             compositor's broadcast never reached the terminal's translator\n{log}"
        ))?;
    eprintln!("  [desktop] the wizard ran three processes below the compositor");
    Ok(())
}

/// The host server behind the netd stream tests, where slirp's `10.0.2.2`
/// lands. Each connection asks in nine bytes — a mode, then a little-endian
/// length — and is served by the mode (`Ask` in
/// `tests/toyos-rust-tests/src/netd_stream.rs`, the other half of this
/// agreement):
///
/// - [`Self::STREAM`]: exactly that many of the guest's `stream_byte` pattern,
///   then its write side is closed.
/// - [`Self::HELD`]: the same bytes, and the connection held open with no FIN
///   until [`Self::finish`].
/// - [`Self::DIAL`]: a connection of the host's own to the guest's forwarded
///   port, written until it is refused; then this connection's write side is
///   closed, which is what tells the guest the forwarded one ended.
///
/// The judgement is the guest's; this side reports only what it sent to each
/// connection and how its sending ended.
///
/// **Nothing here outlives [`PatternServer::finish`].** `accept` is woken by a
/// connection of the server's own, and every connection still reading or
/// writing is shut down under it; the socket timeouts bound only a harness that
/// never reaches `finish`.
struct PatternServer {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    open: std::sync::Arc<std::sync::Mutex<Vec<std::net::TcpStream>>>,
    acceptor: thread::JoinHandle<Vec<thread::JoinHandle<Result<u64, String>>>>,
}

impl PatternServer {
    /// Longest a connection's read or write may stall before `finish` exists
    /// to end it: the guest's own run bound.
    const STALL: Duration = Duration::from_secs(120);

    const STREAM: u8 = 0;
    const HELD: u8 = 1;
    const DIAL: u8 = 2;

    /// The guest program's `stream_byte`, the other half of one agreement.
    fn stream_byte(pos: u64) -> u8 {
        let group = (pos >> 4) as u32;
        match pos & 15 {
            k @ 0..=3 => (group >> (8 * k)) as u8,
            _ => 0xC3,
        }
    }

    /// `forward` is the host port QEMU forwards to the guest, which
    /// [`Self::DIAL`] connects to.
    fn start(forward: Option<u16>) -> Result<Self, String> {
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("bind the host server: {e}"))?;
        let port = listener.local_addr().map_err(|e| format!("the host server's port: {e}"))?.port();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let open = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (stop_seen, open_kept) = (stop.clone(), open.clone());
        let acceptor = thread::spawn(move || {
            let mut served = Vec::new();
            for stream in listener.incoming() {
                if stop_seen.load(Ordering::Acquire) {
                    break;
                }
                let stream = match stream {
                    Ok(s) => s,
                    Err(e) => {
                        served.push(thread::spawn(move || Err(format!("accept: {e}"))));
                        continue;
                    }
                };
                match stream.try_clone() {
                    Ok(kept) => open_kept.lock().expect("the open list").push(kept),
                    Err(e) => {
                        served.push(thread::spawn(move || Err(format!("keep the connection: {e}"))));
                        continue;
                    }
                }
                let open = open_kept.clone();
                served.push(thread::spawn(move || Self::serve(stream, forward, &open)));
            }
            served
        });
        Ok(Self { port, stop, open, acceptor })
    }

    fn serve(
        mut stream: std::net::TcpStream,
        forward: Option<u16>,
        open: &std::sync::Mutex<Vec<std::net::TcpStream>>,
    ) -> Result<u64, String> {
        use std::io::Read;
        stream.set_read_timeout(Some(Self::STALL)).map_err(|e| format!("read timeout: {e}"))?;
        stream.set_write_timeout(Some(Self::STALL)).map_err(|e| format!("write timeout: {e}"))?;
        let mut ask = [0u8; 9];
        stream.read_exact(&mut ask).map_err(|e| format!("read what the guest asks: {e}"))?;
        let total = u64::from_le_bytes(ask[1..].try_into().expect("eight bytes"));
        match ask[0] {
            Self::STREAM | Self::HELD => {}
            Self::DIAL => {
                let forward = forward.ok_or("the guest asked for a dial and this boot forwards no port")?;
                let written = Self::dial(forward, open)?;
                stream.shutdown(std::net::Shutdown::Write).map_err(|e| format!("close the stream: {e}"))?;
                return Ok(written);
            }
            mode => return Err(format!("the guest asked for mode {mode}, which this server does not serve")),
        }
        let mut chunk = vec![0u8; 65536];
        let mut sent = 0u64;
        while sent < total {
            let n = chunk.len().min((total - sent) as usize);
            for (i, b) in chunk[..n].iter_mut().enumerate() {
                *b = Self::stream_byte(sent + i as u64);
            }
            stream.write_all(&chunk[..n]).map_err(|e| format!("send at {sent} of {total}: {e}"))?;
            sent += n as u64;
        }
        if ask[0] == Self::STREAM {
            stream.shutdown(std::net::Shutdown::Write).map_err(|e| format!("close the stream: {e}"))?;
        }
        Ok(sent)
    }

    /// Connect to the guest through `forward` and write until the connection
    /// is refused, answering how many bytes it took first.
    fn dial(forward: u16, open: &std::sync::Mutex<Vec<std::net::TcpStream>>) -> Result<u64, String> {
        let mut dialled = std::net::TcpStream::connect(("127.0.0.1", forward))
            .map_err(|e| format!("dial the guest's forwarded port: {e}"))?;
        dialled.set_write_timeout(Some(Self::STALL)).map_err(|e| format!("write timeout: {e}"))?;
        let kept = dialled.try_clone().map_err(|e| format!("keep the dialled connection: {e}"))?;
        open.lock().expect("the open list").push(kept);
        let chunk = [0u8; 4096];
        let mut written = 0u64;
        loop {
            match dialled.write(&chunk) {
                Ok(0) => return Ok(written),
                Ok(n) => written += n as u64,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                    return Err(format!("the dialled connection took {written} bytes and then nothing for {:?}", Self::STALL));
                }
                Err(_) => return Ok(written),
            }
        }
    }

    /// Stop accepting, end every connection still open, and answer how each
    /// one's sending ended, in the order they were accepted.
    fn finish(self) -> Vec<Result<u64, String>> {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::Release);
        // The wake for `accept`: refused only if the listener is already gone,
        // which means the acceptor has already returned.
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        let served = self.acceptor.join().expect("the host server's acceptor panicked");
        for stream in self.open.lock().expect("the open list").iter() {
            // One half at a time: once the peer has sent its FIN, macOS refuses
            // `Both` whole (`ENOTCONN`) and shuts neither, leaving a writer
            // blocked. A half refused here is one already ended, with nothing
            // blocked on it.
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let _ = stream.shutdown(std::net::Shutdown::Read);
        }
        served
            .into_iter()
            .map(|t| t.join().unwrap_or_else(|_| Err("a host connection's thread panicked".to_string())))
            .collect()
    }
}

/// A UDP echo on the host, where slirp's `10.0.2.2` lands: every datagram goes
/// back to its sender as it came.
///
/// **Nothing here outlives [`UdpEcho::finish`]**, whose own datagram is the
/// wake that ends the loop.
struct UdpEcho {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    echo: thread::JoinHandle<Result<u64, String>>,
}

impl UdpEcho {
    fn start() -> Result<Self, String> {
        use std::sync::atomic::Ordering;
        let socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).map_err(|e| format!("bind the UDP echo: {e}"))?;
        let port = socket.local_addr().map_err(|e| format!("the UDP echo's port: {e}"))?.port();
        socket.set_read_timeout(Some(PatternServer::STALL)).map_err(|e| format!("read timeout: {e}"))?;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_seen = stop.clone();
        let echo = thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            let mut echoed = 0u64;
            loop {
                let (n, from) = socket.recv_from(&mut buf).map_err(|e| format!("UDP echo receive: {e}"))?;
                if stop_seen.load(Ordering::Acquire) {
                    return Ok(echoed);
                }
                socket.send_to(&buf[..n], from).map_err(|e| format!("UDP echo send: {e}"))?;
                echoed += 1;
            }
        });
        Ok(Self { port, stop, echo })
    }

    /// Stop echoing, and answer how many datagrams went back.
    fn finish(self) -> Result<u64, String> {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::Release);
        let waker = std::net::UdpSocket::bind(("127.0.0.1", 0)).map_err(|e| format!("bind the echo's wake: {e}"))?;
        waker.send_to(&[], ("127.0.0.1", self.port)).map_err(|e| format!("wake the echo: {e}"))?;
        self.echo.join().unwrap_or_else(|_| Err("the UDP echo panicked".to_string()))
    }
}

/// What one [`netcase_against_host`] run leaves: the guest's result, the
/// console it ran beside, and how the host's sending ended on each connection.
struct HostRun {
    result: qemu::TestResult,
    console: String,
    sent: Vec<Result<u64, String>>,
}

/// Boot `tests/netcase` with the one guest program `name`, wait for netd, and
/// run it against a fresh [`PatternServer`], whose port is its first argument
/// and `args` the rest. `forward` forwards a host port to the guest's TCP 22
/// for [`PatternServer::DIAL`].
fn netcase_against_host(
    rust_bins: &[(String, Vec<u8>)],
    name: &str,
    forward: bool,
    args: &str,
) -> Result<HostRun, String> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
    let bins: Vec<(String, Vec<u8>)> = rust_bins.iter().filter(|(n, _)| n == name).cloned().collect();
    if bins.is_empty() {
        return Err(format!("{name} was not built"));
    }
    let options = BootOptions {
        profile: qemu::Profile::Headless,
        ssh_port: forward.then(qemu::free_host_port),
        ..Default::default()
    };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
        return Err("this test needs a NIC and the profile has none".to_string());
    }
    let server = PatternServer::start(options.ssh_port)?;
    let port = server.port;
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
    let mut console = qemu.boot_log().to_string();
    let up = await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up");
    let result = up.map(|_| qemu.run_test(&format!("test_rs_{name} {port}{args}"), Duration::from_secs(120)));
    let sent = server.finish();
    let result = result.map_err(|e| format!("netd never came up, so nothing below means anything: {e}"))?;
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}\nthe host's connections ended {sent:?}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!(
            "{name} exited {:?}:\n{}\nthe host's connections ended {sent:?}",
            result.exit_code, result.stdout
        ));
    }
    console.push_str(&result.serial);
    Ok(HostRun { result, console, sent })
}

/// A receiver that stops reading until its pipe is full still gets every byte
/// of a stream past it, from the guest's own byte-for-byte comparison.
fn netd_slow_reader(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let HostRun { result, sent, .. } = netcase_against_host(rust_bins, "netd_slow_reader", false, "")?;
    let [Ok(sent)] = sent.as_slice() else {
        return Err(format!("the host server's connections ended {sent:?}, not one whole stream"));
    };
    let ok = format!("netd_slow_reader: ok bytes={sent}");
    if !result.stdout.lines().any(|l| l.trim_end().ends_with(&ok)) {
        return Err(format!("the host sent {sent} bytes and the guest never said {ok:?}:\n{}", result.stdout));
    }
    eprintln!("  [netcase] a reader a whole pipe behind got all {sent} bytes, each right");
    Ok(())
}

/// Bytes a reader holds back in the socket, past a full pipe, move when the
/// reader makes room — with the peer holding the connection open and silent,
/// so nothing but the pipe's own room can be what moved them.
fn netd_held_open(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let HostRun { result, sent, .. } = netcase_against_host(rust_bins, "netd_held_open", false, "")?;
    let [Ok(sent)] = sent.as_slice() else {
        return Err(format!("the host server's connections ended {sent:?}, not one held stream"));
    };
    let ok = format!("netd_held_open: ok bytes={sent},");
    let Some(line) = result.stdout.lines().find(|l| l.contains(&ok)) else {
        return Err(format!("the host sent {sent} bytes and the guest never said {ok:?}:\n{}", result.stdout));
    };
    eprintln!("  [netcase] {}", line.trim_end());
    Ok(())
}

/// A UDP datagram the client's pipe will not take whole ends that socket by
/// name, and nothing else: another socket still gets its datagram.
fn netd_udp_refused(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let echo = UdpEcho::start()?;
    let run = netcase_against_host(rust_bins, "netd_udp_refused", false, &format!(" {}", echo.port));
    let echoed = echo.finish();
    let HostRun { result, console, .. } = run.map_err(|e| format!("{e}\nthe UDP echo ended {echoed:?}"))?;
    let echoed = echoed?;
    if !result.stdout.lines().any(|l| l.trim_end().ends_with("netd_udp_refused: ok")) {
        return Err(format!("the guest never said it was done:\n{}", result.stdout));
    }
    let named = "its receive pipe took ";
    if !console.contains(named) {
        return Err(format!("netd ended a UDP socket without a `{named}` line:\n{console}"));
    }
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    eprintln!("  [netcase] a datagram its pipe would not take whole ended that socket alone ({echoed} echoed)");
    Ok(())
}

/// A socket bound to 0.0.0.0 receives the unicast reply to what it sent: the
/// guest's comparison of the echo's reply with its datagram is the verdict.
fn netd_udp_any_address(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let echo = UdpEcho::start()?;
    let run = netcase_against_host(rust_bins, "netd_udp_any_address", false, &format!(" {}", echo.port));
    let echoed = echo.finish();
    let HostRun { result, console, .. } = run.map_err(|e| format!("{e}\nthe UDP echo ended {echoed:?}"))?;
    let echoed = echoed?;
    if !result.stdout.lines().any(|l| l.trim_end().ends_with("netd_udp_any_address: ok")) {
        return Err(format!("the guest never said it was done:\n{}", result.stdout));
    }
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    eprintln!("  [netcase] a socket bound to 0.0.0.0 got the echo's reply ({echoed} echoed)");
    Ok(())
}

/// A real name, resolved through the guest's user network: QEMU's `10.0.2.3`
/// forwards each query to this host's own resolver, so what `host` prints in
/// the guest is judged by what this host's resolver answers for the same name,
/// a resolver this tree did not write. A name under `.invalid` has none (RFC 6761 §6.4),
/// and is answered as none rather than as a timeout.
fn dns_resolve() -> Result<(), String> {
    use std::collections::BTreeSet;
    use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};

    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
    let options = BootOptions { profile: qemu::Profile::Headless, ..Default::default() };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
        return Err("this test needs a NIC and the profile has none".to_string());
    }
    let oracle: BTreeSet<Ipv4Addr> = (DNS_REAL_NAME, 0)
        .to_socket_addrs()
        .map_err(|e| format!("this host's resolver would not resolve {DNS_REAL_NAME}: {e}"))?
        .filter_map(|a| match a.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .collect();
    if oracle.is_empty() {
        return Err(format!("this host's resolver has no IPv4 address for {DNS_REAL_NAME}"));
    }
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
    let mut console = qemu.boot_log().to_string();
    await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up")?;

    let found = qemu.run_test(&format!("host {DNS_REAL_NAME}"), Duration::from_secs(60));
    if let Some(err) = &found.error {
        return Err(format!("{err}\n{}", found.stdout));
    }
    if found.exit_code != Some(0) {
        return Err(format!("host {DNS_REAL_NAME} exited {:?}:\n{}{}", found.exit_code, found.stdout, found.serial));
    }
    let said = format!("{DNS_REAL_NAME} has address ");
    let guest: BTreeSet<Ipv4Addr> = found
        .stdout
        .lines()
        .filter_map(|l| l.trim_end().split_once(&said).and_then(|(_, a)| a.parse().ok()))
        .collect();
    if guest != oracle {
        return Err(format!(
            "the guest resolved {DNS_REAL_NAME} to {guest:?} and this host to {oracle:?}:\n{}",
            found.stdout
        ));
    }

    let missing = qemu.run_test(&format!("host {DNS_NO_NAME}"), Duration::from_secs(60));
    if let Some(err) = &missing.error {
        return Err(format!("{err}\n{}", missing.stdout));
    }
    let output = format!("{}{}", missing.stdout, missing.serial);
    if missing.exit_code != Some(1) || output.contains(" has address ") || !output.contains(DNS_NO_ADDRESS) {
        return Err(format!(
            "host {DNS_NO_NAME} exited {:?} and did not say {DNS_NO_ADDRESS:?}:\n{output}",
            missing.exit_code
        ));
    }
    console.push_str(&found.serial);
    console.push_str(&missing.serial);
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    eprintln!("  [netcase] {DNS_REAL_NAME} resolved in the guest to {guest:?}, as this host resolves it");
    eprintln!("  [netcase] {DNS_NO_NAME} resolved to no address, not to a timeout");
    Ok(())
}

const DNS_REAL_NAME: &str = "dns.google";

/// A name no resolver can find (RFC 6761 §6.4).
const DNS_NO_NAME: &str = "doesnotexist.invalid";

/// What std's `lookup_host` says for a name netd answered with no address,
/// and so what `host` prints for one: its word, not netd's, and not a
/// timeout's.
const DNS_NO_ADDRESS: &str = "no results";

/// netd's loop lets a lookup go the moment its client hangs up or speaks
/// again, and ends one nobody answers when its schedule does: the netcase boot
/// once it has its lease, with every frame it sends from then on held by
/// QEMU, so no query reaches its resolver. The verdict is the guest's, from
/// netd's answers.
fn netd_lookup_let_go(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    const NAME: &str = "netd_lookup_let_go";
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
    let bins: Vec<(String, Vec<u8>)> = rust_bins.iter().filter(|(n, _)| n == NAME).cloned().collect();
    if bins.is_empty() {
        return Err(format!("{NAME} was not built"));
    }
    let options = BootOptions { profile: qemu::Profile::Headless, qmp: true, ..Default::default() };
    if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
        return Err("this test needs a NIC and the profile has none".to_string());
    }
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
    let mut console = qemu.boot_log().to_string();
    await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up")?;
    // A lookup before the lease is refused as having no server.
    await_marker(&mut qemu, &mut console, "netd: DHCP: lease ", "netd's lease")?;
    // One lookup before the frames are held, whatever it is answered, has
    // netd learn its resolver's link address. Every query after it leaves and
    // is lost, so no link-address retry wakes netd's loop, and only the
    // resolver's own wake carries a lookup to its end.
    let primed = qemu.run_test(&format!("host {DNS_NO_NAME}"), Duration::from_secs(60));
    if let Some(err) = &primed.error {
        return Err(format!("the lookup before the frames were held: {err}\n{}", primed.stdout));
    }
    console.push_str(&primed.serial);
    qemu::QmpDevices::open(qemu.qmp_socket()).hold_outbound("net0");
    let result = qemu.run_test(&format!("test_rs_{NAME}"), Duration::from_secs(180));
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    if result.exit_code != Some(0) || !result.stdout.lines().any(|l| l.trim_end().ends_with("netd_lookup_let_go: ok")) {
        return Err(format!("{NAME} exited {:?}:\n{}{}", result.exit_code, result.stdout, result.serial));
    }
    let spoke = "it spoke again before its answer";
    if !result.serial.contains(spoke) {
        return Err(format!("netd dropped a client that spoke again without a `{spoke}` line:\n{}", result.serial));
    }
    console.push_str(&result.serial);
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    for line in result.stdout.lines().filter(|l| l.contains("netd_lookup_let_go: ")) {
        eprintln!("  [netcase] {}", line.trim_end());
    }
    Ok(())
}

/// Two boots of one image draw different first DHCP transaction IDs, because
/// netd seeds smoltcp's random source from the kernel's; seeded as smoltcp's
/// `Config::new` leaves it, every boot draws the same one. Read off the wire
/// QEMU's user network was handed, where a server reads them.
fn netd_seeds_its_stack() -> Result<(), String> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
    let mut firsts = Vec::new();
    for boot in 0..2 {
        let dump = common::lane::dir().join(format!("seed-{boot}.pcap"));
        let _ = fs::remove_file(&dump);
        let options =
            BootOptions { profile: qemu::Profile::Headless, wire_dump: Some(dump.clone()), ..Default::default() };
        if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
            return Err("this test needs a NIC and the profile has none".to_string());
        }
        let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
        let mut console = qemu.boot_log().to_string();
        await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up")?;
        // QEMU owns the pcap while it runs.
        drop(qemu);
        let frames = fs::read(&dump).map_err(|e| format!("{}: {e}", dump.display()))?;
        let _ = fs::remove_file(&dump);
        serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
        let ids = toyos_build::lan::dhcp_transaction_ids(&frames)?;
        let first = *ids.first().ok_or_else(|| format!("boot {boot} put no DHCP frame on the wire"))?;
        firsts.push(first);
    }
    if firsts[0] == firsts[1] {
        return Err(format!("both boots' first DHCP transaction ID was {:#010x}", firsts[0]));
    }
    eprintln!("  [netcase] the two boots' first DHCP transaction IDs: {:#010x} and {:#010x}", firsts[0], firsts[1]);
    Ok(())
}

/// Client pipes netd cannot use, or loses under it, end that client's
/// connection and never netd: the guest's round trip after each case is the
/// verdict that netd survived it. This side carries what the guest cannot
/// see — that netd named each refusal and that no program panicked.
fn netd_refused_pipes(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let HostRun { result, console, .. } = netcase_against_host(rust_bins, "netd_refused_pipes", true, "")?;
    if !result.stdout.lines().any(|l| l.trim_end().ends_with("netd_refused_pipes: ok")) {
        return Err(format!("the guest never said it was done:\n{}", result.stdout));
    }
    for named in [
        "netd: resetting a connection — its receive pipe refused netd: PermissionDenied",
        "netd: resetting a connection — its send pipe refused netd: PermissionDenied",
        "its notify pipe refused netd: PermissionDenied",
        "netd: resetting a connection — its receive pipe refused netd: InvalidArgument",
        "its notify pipe refused netd: InvalidArgument",
    ] {
        if !console.contains(named) {
            return Err(format!("netd refused a client's pipe without a `{named}` line:\n{console}"));
        }
    }
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    for line in result.stdout.lines().filter(|l| l.contains(", and a round trip after it")) {
        eprintln!("  [netcase] {}", line.trim_end());
    }
    eprintln!("  [netcase] six refused client handles cost netd nothing, and each was named");
    Ok(())
}

/// An accept netd refuses for room leaves its owner a wake for the connection
/// it left: the guest's wakes are the verdict. This side carries that netd named the
/// refusal for room and that no program panicked.
fn netd_refused_accept(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let HostRun { result, console, .. } = netcase_against_host(rust_bins, "netd_refused_accept", true, "")?;
    if !result.stdout.lines().any(|l| l.trim_end().ends_with("netd_refused_accept: ok")) {
        return Err(format!("the guest never said it was done:\n{}", result.stdout));
    }
    if !console.contains("netd: refusing accept, ") {
        return Err(format!("netd refused an accept for room without saying so:\n{console}"));
    }
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    eprintln!("  [netcase] an accept refused for room left a wake once room returned");
    Ok(())
}

/// Ctrl+Alt+D at a live desktop: every CPU answers, and the two halves of the
/// report agree.
///
/// The instrument `issues/diagnostics/` files against, built because QEMU cannot
/// stage the T14's audio wedge and a question the owner can answer beats a fix
/// nobody can verify. Until this landed the dump listed the *calling* CPU's
/// parked threads and named them by scheduler key, so it could confirm a park
/// and never rule one out — and the three states that look identical from
/// outside (parked on a deadline that did not fire, parked on a deadline
/// nothing could reach, held by no CPU at all) were not distinguishable at all.
///
/// Eight CPUs, because "machine-wide" is not testable at the suite's default of
/// two: one CPU short of the whole machine is what the old dump already did.
///
/// **The verdict is the instrument, not the guest's health.** A deadline that
/// has passed and whose pass has not yet run is a legitimate microsecond-wide
/// state, so asserting zero of them would be asserting a race. What is asserted
/// is that the report is complete and that its halves cannot disagree: every
/// CPU is present, the deadline classes sum to the parked count, and the
/// process table knows at least as many threads as the schedulers hold.
fn blocked_dump() -> Result<(), String> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/desktopaudiocase");
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        smp: 8,
        qmp: true,
        ready_marker: "compositor: ready",
        // `Drained::Bytes`; off the shipping kernel, and implies fast-health
        // and edge-race.
        kernel_params: &["i8042-trace"],
        ..Default::default()
    };
    metal_sim_argv_check(&qemu::profile_argv(&options))?;
    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
    let mut log = qemu.boot_log().to_string();
    // No panel row under a compositor; the kernel's drain report is the answer.
    let ack = Drained::Bytes;
    if let Err(why) = shell_answers(&mut qemu, &mut log, &ack) {
        return Err(format!(
            "{why}\nnothing typed at the terminal window reached a shell:\n{log}"
        ));
    }

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
    await_marker_new(&mut qemu, &mut log, "=== end of dump ===", before, "the whole report")
        .map_err(|why| format!(
            "{why}\nCtrl+Alt+D produced no complete report:\n{}",
            &log[before..]
        ))?;
    let report = log[before..].to_string();

    // Every CPU printed its own line. This is the whole of "machine-wide": the
    // count in the summary is derived, these are the CPUs actually answering.
    let missing: Vec<usize> =
        (0..8).filter(|c| !report.contains(&format!("cpu{c} running"))).collect();
    if !missing.is_empty() {
        return Err(format!(
            "cpu(s) {missing:?} never reported — the dump reached {} of 8:\n{report}",
            8 - missing.len()
        ));
    }
    if !report.contains("8/8 cpu(s) answered") {
        return Err(format!("the report does not claim a whole machine:\n{report}"));
    }
    // On a settled desktop the table is free, so the half of the verdict that
    // only the census can produce must be there. A report that answered two of
    // three questions is worth having on the owner's panel and is not worth
    // accepting from a gate.
    if !report.contains(" unheld, ") || !report.contains(" never ran") {
        return Err(format!(
            "the verdict lost its census half on a settled machine:\n{report}"
        ));
    }

    // A parked line names a process, not a scheduler key.
    let named = report
        .lines()
        .filter(|l| l.contains("pid=") && l.contains("tid=") && l.contains(" parked "))
        .count();
    if named == 0 {
        return Err(format!("no parked task was named by pid and tid:\n{report}"));
    }

    // **Every kernel thread, by name.** They are almost always blocked, so
    // the parked lines above carry them as a pid and a tid and nothing else —
    // and on a machine that has gone quiet the question is *which* one is
    // stuck. `sched::dump`'s census tags a kernel thread whatever it is doing.
    //
    // Matched with the ` cpu=` that follows the name on the census line, because
    // a bare name appears in every one of these programs' own log lines and
    // `/system/bin/init` speaks in a program's name before that program runs
    // (`tests/CLAUDE.md`).
    let unnamed: Vec<&str> = ["klogd", "iod"]
        .into_iter()
        .filter(|name| !report.contains(&format!(" {name} cpu=")))
        .collect();
    if !unnamed.is_empty() {
        return Err(format!(
            "the report never names kernel thread(s) {unnamed:?}, so it cannot say which \
             of them is stuck:\n{report}"
        ));
    }

    // The two halves must agree, which is what makes the verdict mean anything:
    // every parked task falls into exactly one deadline class, and every task a
    // scheduler holds is a thread the process table knows.
    let parked = dump_field(&report, "== sched:", "parked")?;
    let classes = dump_field(&report, "== deadlines:", "event-only,")?
        + dump_field(&report, "== deadlines:", "pending,")?
        + dump_field(&report, "== deadlines:", "OVERDUE,")?
        + dump_field(&report, "== deadlines:", "ABSURD")?;
    if parked != classes {
        return Err(format!(
            "{parked} parked task(s) but {classes} classified — the report contradicts \
             itself:\n{report}"
        ));
    }
    let threads = dump_field(&report, "== census:", "thread(s)")?;
    if threads < parked {
        return Err(format!(
            "the schedulers hold {parked} task(s) and the process table knows {threads} \
             thread(s) — the census cannot see what the CPUs do:\n{report}"
        ));
    }

    let verdict = report
        .lines()
        .find(|l| l.contains("== VERDICT:"))
        .ok_or_else(|| format!("no verdict line:\n{report}"))?;
    eprintln!(
        "  [dump] {threads} threads, {parked} parked, all 8 cpus answered;{}",
        verdict.split("VERDICT:").nth(1).unwrap_or("").trim_end()
    );
    Ok(())
}

/// The number the report writes immediately before `word`, on the line that
/// carries `marker`. Read from the word a person sees rather than from a
/// column, so a reordered line does not silently read the wrong field.
fn dump_field(report: &str, marker: &str, word: &str) -> Result<u32, String> {
    let line = report
        .lines()
        .find(|l| l.contains(marker))
        .ok_or_else(|| format!("no {marker:?} line in the report:\n{report}"))?;
    let head = line
        .split(word)
        .next()
        .filter(|h| h.len() < line.len())
        .ok_or_else(|| format!("no {word:?} on {line:?}"))?;
    head.split_whitespace()
        .next_back()
        .and_then(|w| w.parse().ok())
        .ok_or_else(|| format!("no number before {word:?} on {line:?}"))
}

/// The direct regression for the readiness defect: a stimulus that produces
/// bytes and no events must produce no wake. Pause is that stimulus — six
/// bytes, deliberately swallowed.
///
/// It drives `test_rs_i8042_keyboard`, for the userland half of the assertion.
///
/// **The zero-event drain is arranged, not hoped for.** What a drain carries is
/// whatever the ISR found in the ring, so a host that injects on a wall clock
/// is asserting on a batching it does not control: a guest that does not drain
/// between the Pause and the key that follows it takes both in one drain, and
/// this test's whole precondition is gone. It also puts more bytes in flight
/// than [`QEMU_PS2_QUEUE`] holds — twenty against sixteen — and the device
/// drops the excess silently, one byte at a time. So each piece goes out only
/// once the guest has reported what the piece before it produced: the Pause is
/// paid by a drain the driver logged, a real key by its two `kev` lines. Six
/// bytes outstanding at most, and a slow guest costs wall clock.
fn i8042_no_spurious_wake(boot: &mut Boot) -> Result<(), String> {
    /// What the guest owes for one injected group before the next goes out.
    enum Owed {
        /// A drain the driver reported. The only thing a swallowed sequence
        /// produces, and therefore the only thing that can pay for one.
        Drain,
        /// `n` `kev` lines: a real key's make and break.
        Keys(usize),
    }

    const SCRIPT: &[(&[(&str, bool)], Owed)] = &[
        (&[("pause", true), ("pause", false)], Owed::Drain),
        (&[("a", true), ("a", false)], Owed::Keys(2)),
        (&[("pause", true), ("pause", false)], Owed::Drain),
        (&[("a", true), ("a", false)], Owed::Keys(2)),
        // The sentinel the guest exits on; see [`send_i8042_sentinel`].
        (&[("end", true), ("end", false)], Owed::Keys(2)),
    ];

    let qemu = &mut boot.qemu;
    let sent = std::cell::Cell::new(0usize);
    let result = {
        let mut input: Option<qemu::QmpInput> = None;
        let mut drains = 0usize;
        let mut keys = 0usize;
        // The counters as they stood when the group still outstanding was sent.
        let mut at_drains = 0usize;
        let mut at_keys = 0usize;
        qemu.run_test_paced(
            "test_rs_i8042_keyboard",
            Duration::from_secs(20),
            |socket, line| {
                if line.contains(I8042_READY) {
                    input = Some(qemu::QmpInput::open(
                        socket.expect("i8042_no_spurious_wake needs BootOptions { qmp }"),
                    ));
                }
                if trace_keys(line).is_some() {
                    drains += 1;
                }
                if line.contains("kev usage=") {
                    keys += 1;
                }
                let Some(input) = input.as_mut() else { return };
                let paid = match SCRIPT.get(sent.get().wrapping_sub(1)) {
                    None => true,
                    Some((_, Owed::Drain)) => drains > at_drains,
                    Some((_, Owed::Keys(n))) => keys >= at_keys + n,
                };
                if !paid {
                    return;
                }
                if let Some((group, _)) = SCRIPT.get(sent.get()) {
                    input.keys(group);
                    at_drains = drains;
                    at_keys = keys;
                    sent.set(sent.get() + 1);
                }
            },
        )
    };
    let sent = sent.get();
    if let Some(err) = &result.error {
        // The guard, not the verdict: the host is waiting on the guest here.
        return Err(format!(
            "{STALLED} {err} — {sent} of {} groups sent when the host gave up waiting for what \
             the last one owed\n{}",
            SCRIPT.len(),
            result.stdout
        ));
    }

    let mut zero_event_drains = 0;
    let mut key_drains = 0;
    for line in result.serial.lines() {
        let Some(keys) = trace_keys(line) else { continue };
        let woke = line.contains("woke_kb=1");
        if keys == 0 {
            zero_event_drains += 1;
            if woke {
                return Err(format!("a drain with no events woke the queue: {line}"));
            }
        } else {
            key_drains += 1;
            if !woke {
                return Err(format!("a drain with events did not wake the queue: {line}"));
            }
        }
    }
    if zero_event_drains == 0 {
        // Not "the stimulus never landed": every Pause above was paid for by a
        // drain before the next injection went out, so one *did* land and one
        // drain did report it. What is left is a drain that took the Pause and
        // produced an event out of it — which is the readiness defect itself.
        return Err(format!(
            "{sent} groups sent, each after the last was reported, and no drain produced zero \
             events — every drain that took a swallowed Pause claimed an event:\n{}",
            result.serial
        ));
    }
    if key_drains == 0 {
        return Err(format!("no drain produced any event:\n{}", result.serial));
    }
    // And the swallowed bytes stayed swallowed all the way out.
    let events = parse_key_events(&result.stdout);
    if events.iter().any(|e| e.usage == 0x48) {
        return Err(format!("Pause reached userland as a key: {events:?}"));
    }
    if !events.iter().any(|e| e.usage == 0x04) {
        return Err(format!("the real key never arrived: {events:?}"));
    }
    eprintln!(
        "  [i8042] {zero_event_drains} zero-event drains, none woke; {key_drains} real ones, all \
         did; {sent} groups, each paid for before the next"
    );
    Ok(())
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

/// A PS/2 pointer packet. Three bytes, because the driver's aux init sends no
/// IntelliMouse knock and QEMU therefore frames a plain mouse.
const MOUSE_PACKET: usize = 3;

/// How far the host may run ahead of the guest while it feeds the framer.
///
/// A packet the guest has reported is a packet whose bytes have left the
/// device's queue, so the lead bounds that queue's occupancy — which is the
/// only thing that makes an injected command a packet. Past the bound QEMU
/// stops queueing motion and starts *accumulating* it, and the merged deltas
/// come back as one packet or, if they cancel, as none at all.
const MOUSE_LEAD: usize = 4;

const _: () = assert!(
    MOUSE_PACKET * MOUSE_LEAD <= QEMU_PS2_QUEUE,
    "the lead outruns QEMU's PS/2 queue, which merges the motion it cannot hold"
);

/// Moves the staged merge puts in one command: more than one, and few enough
/// that their sum stays inside the packet's signed byte.
const MERGE_MOTIONS: usize = 4;

/// The TrackPoint path, and a thousand packets through the framer after it,
/// each sent only once the one before it has come out of the guest.
///
/// The pacing is the design, and [`MOUSE_LEAD`] is what makes it one: a host
/// injecting at its own speed measures how fast the guest drains and reads the
/// shortfall as a driver defect. Staying inside what the device holds leaves no
/// loss to tolerate: every packet injected is a packet that arrived, or the run
/// stalls and says how far it got. It is also what makes the driver's
/// `discarded`/`dropped` counters mean something a slow guest cannot account
/// for.
fn i8042_mouse(boot: &mut Boot) -> Result<(), String> {
    let qemu = &mut boot.qemu;
    let boot = qemu.boot_log().to_string();
    // **The whole line, because its tail is the verdict.** The unmask's result
    // used to be discarded and the line stopped at the APIC, so a GSI that
    // never unmasked printed exactly what a working one did — and every packet
    // this test injects below would then arrive nowhere, which is the check
    // that the word is not just a word.
    let Some(aux) = boot.lines().find(|l| l.contains("i8042: aux rate=100")) else {
        return Err(format!("the TrackPoint path never came up:\n{boot}"));
    };
    if !aux.ends_with(" on") {
        return Err(format!(
            "the aux line does not end in the unmask's verdict, so a masked GSI reads as a \
             live one: {aux:?}"
        ));
    }

    const BURST: usize = 1000;
    let injected = std::cell::Cell::new(0usize);
    let arrived = std::cell::Cell::new(0usize);
    let result = {
        let mut input: Option<qemu::QmpInput> = None;
        let mut burst = 0usize;
        let mut clicked = false;
        let mut merged = false;
        let mut counted = false;
        let mut ended = false;
        qemu.run_test_paced("test_rs_i8042_mouse", Duration::from_secs(60), |socket, line| {
            if line.contains("===I8042_MOUSE_READY===") {
                let mut open =
                    qemu::QmpInput::open(socket.expect("i8042_mouse needs BootOptions { qmp }"));
                // Off the origin first: the position clamps at 0, so a
                // move up from there would be invisible.
                open.mouse(100, 100, None);
                open.mouse(40, -30, None);
                open.mouse(0, 0, Some(("left", true)));
                open.mouse(0, 0, Some(("left", false)));
                injected.set(4);
                input = Some(open);
            }
            if line.contains("mev buttons=") {
                arrived.set(arrived.get() + 1);
            }
            counted |= clicked && line.contains("discarded");
            let Some(input) = input.as_mut() else { return };
            if ended {
                return;
            }
            // One command per packet, because QEMU syncs input once per
            // command: `BURST` commands is `BURST` packets and three times that
            // many bytes through the framer. Refilling the window on every
            // arrival is what keeps the stream continuous under the pacing.
            while burst < BURST && injected.get() < arrived.get() + MOUSE_LEAD {
                input.mouse(if burst.is_multiple_of(2) { 1 } else { -1 }, 0, None);
                burst += 1;
                injected.set(injected.get() + 1);
            }
            if burst < BURST || arrived.get() < injected.get() {
                return;
            }
            if !clicked {
                input.mouse(0, 0, Some(("left", true)));
                input.mouse(0, 0, Some(("left", false)));
                injected.set(injected.get() + 2);
                clicked = true;
                return;
            }
            // What [`MOUSE_LEAD`] exists to stay clear of, staged where it can
            // do no harm: the queue is empty here, so the merge is the device's
            // one-sync-per-command rule and nothing else.
            if !merged {
                input.mouse_merged(1, MERGE_MOTIONS);
                injected.set(injected.get() + 1);
                merged = true;
                return;
            }
            // The driver reports its counters from a scheduler pass, and the
            // client polling its handle is what keeps passes running: the line has
            // to arrive before the client is told to stop.
            if !counted {
                return;
            }
            // The only right button in the sequence, and the client's signal to
            // exit. It stops on the release, so both halves are printed and the
            // framing assertion still reads a pointer with nothing held down.
            input.mouse(0, 0, Some(("right", true)));
            input.mouse(0, 0, Some(("right", false)));
            injected.set(injected.get() + 2);
            ended = true;
        })
    };
    let (injected, arrived) = (injected.get(), arrived.get());
    if let Some(err) = &result.error {
        // The guard, not the count: the pacing means the host is *waiting* on a
        // packet when this fires, so what it has established is that the run
        // stopped, never that the machine dropped one.
        return Err(format!(
            "{STALLED} {err} — {arrived} of the {injected} packets injected had come back out \
             when the host gave up waiting for the next\n{}",
            result.stdout
        ));
    }

    let events = parse_mouse_events(&result.stdout);
    // The host never had more outstanding than the device holds, so a shortfall
    // is a packet the machine lost and never a host that outran it.
    if events.len() != injected {
        return Err(format!(
            "{} pointer events reached userland out of {injected} packets injected, never more \
             than {MOUSE_LEAD} of them ({} bytes) outstanding against a {QEMU_PS2_QUEUE}-byte \
             device queue",
            events.len(),
            MOUSE_LEAD * MOUSE_PACKET,
        ));
    }
    // The step one packet moves the pointer, off the first two of the burst.
    let step = (events[5].x as i32 - events[4].x as i32).abs();
    // Third from last: the staged merge, then the right button's two halves.
    let merge = events.len() - 3;
    let jump = (events[merge].x as i32 - events[merge - 1].x as i32).abs();
    if step == 0 || jump != step * MERGE_MOTIONS as i32 {
        return Err(format!(
            "{MERGE_MOTIONS} moves in one command moved the pointer {jump} against a one-move \
             step of {step}: QEMU no longer sums motion between syncs, and `MOUSE_LEAD` is \
             derived from the fact that it does"
        ));
    }
    // A sign error in dy is invisible to any test that only checks
    // "it moved", and the PS/2 wire points the opposite way to the
    // screen — so both directions are asserted separately.
    if !events.windows(2).any(|w| w[1].x > w[0].x) {
        return Err("the pointer never moved right".to_string());
    }
    if !events.windows(2).any(|w| w[1].y < w[0].y) {
        return Err(format!(
            "the pointer never moved up — dy inverted? ys: {:?}",
            events.iter().take(8).map(|e| e.y).collect::<Vec<_>>()
        ));
    }
    // PS/2 bit 0 is left, and so is HID boot-mouse bit 0.
    if !events.iter().any(|e| e.buttons == 0x01) {
        return Err(format!(
            "no left-button-down event; buttons seen: {:?}",
            events.iter().map(|e| e.buttons).collect::<std::collections::BTreeSet<_>>()
        ));
    }
    // And after 3000 bytes of packets the framer is still aligned:
    // the last click is reported as a click, not as motion or as the
    // wrong button.
    let last_press = events.iter().rposition(|e| e.buttons == 0x01);
    let Some(last_press) = last_press else {
        return Err("no button press at all".to_string());
    };
    if events[last_press..].last().map(|e| e.buttons) != Some(0x00) {
        return Err(format!(
            "framing drifted: after the final click the button state is {:?}",
            events.last()
        ));
    }
    // The T14's line, staged. Its log read
    //   `6 bytes, 0 keys, 2 motion, no event from
    //    [aux 0x08, aux 0x06, aux 0x08, aux 0x0e]`
    // on a pointer that was framing perfectly: two whole packets, and
    // the four bytes named were their heads and first body bytes. That
    // sent a field investigation after a desync that had not happened.
    // Three thousand bytes of healthy packets is the same claim with
    // three orders of magnitude more of it: a driver that cannot tell a
    // byte it is holding from a byte it threw away names two thirds of
    // them here.
    let named: Vec<&str> =
        result.serial.lines().filter(|l| l.contains("no event from")).collect();
    if !named.is_empty() {
        return Err(format!(
            "{BURST} clean packets and the driver still named bytes as undecodable:\n{}",
            named.join("\n")
        ));
    }
    // And the counts that say so directly, off the driver's own line. A
    // discard is the byte-level resync and nothing else, so an intact stream
    // owes zero of them — which is what makes any non-zero value on the T14's
    // next boot mean the pointer really did lose the frame. `dropped` is the
    // ring overflowing and `lost edges` an interrupt no pass ever accounted
    // for; [`MOUSE_LEAD`] is what leaves a slow guest unable to produce
    // either.
    let counters = result
        .serial
        .lines()
        .rfind(|l| l.contains("discarded"))
        .ok_or_else(|| format!("the driver never reported its counters:\n{}", result.serial))?;
    for owed in ["0 discarded", "0 overruns", "0 dropped", "0 lost edges"] {
        if !counters.contains(owed) {
            return Err(format!(
                "{injected} packets, none of them sent before the one before it arrived, and the \
                 driver does not report `{owed}`: {counters}"
            ));
        }
    }
    eprintln!("  [i8042] {}", counters.trim());
    eprintln!(
        "  [i8042] {} packets injected, {} out, last button state {:#04x}",
        injected,
        events.len(),
        events.last().unwrap().buttons
    );
    Ok(())
}

/// The compositor's window cap, end to end, on the only config that boots a
/// compositor an in-guest binary can talk to.
///
/// The assertion that matters is not "a refusal arrived" — it is that the
/// number the compositor *derived* from total memory and the screen is the
/// number of windows a client actually gets. A constant on both sides would
/// agree with itself forever; this fails if the derivation and the enforcement
/// ever drift apart.
///
/// Runs before the two clients that abuse the compositor, because a cap is
/// only countable from a desktop with every window still free.
fn metal_sim_window_caps(boot: &mut Boot) -> Result<(), String> {
    // The compositor announces what it derived. Read rather than
    // recomputed here: recomputing it would copy the formula into the
    // test and stop asking whether the compositor uses it. Off the group's
    // console, because the compositor says it once and an earlier member of
    // the group has already drained the line off the wire.
    let _ = await_marker(&mut boot.qemu, &mut boot.console, "compositor: at most ", "the window cap");
    let Some(declared) = boot
        .console
        .lines()
        .find_map(|l| l.split("compositor: at most ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<usize>().ok())
    else {
        return Err(format!(
            "the compositor never said how many windows it would hold:\n{}",
            boot.console
        ));
    };
    if declared == 0 {
        return Err("the compositor derived a cap of zero windows".to_string());
    }

    let result = boot.qemu.run_test("test_rs_window_caps", Duration::from_secs(120));
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!(
            "window_caps exited {:?}:\n{}",
            result.exit_code, result.stdout
        ));
    }

    let Some(granted) = result
        .stdout
        .split("oversized refused, ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<usize>().ok())
    else {
        return Err(format!("window_caps printed no count:\n{}", result.stdout));
    };
    if granted != declared {
        return Err(format!(
            "the compositor declared a cap of {declared} windows and granted \
             {granted} — the derivation and the enforcement disagree:\n{}",
            result.stdout
        ));
    }
    eprintln!("  [metal-sim] compositor cap {declared} windows, {granted} granted then refused");
    Ok(())
}

/// A client that lies about its frame lengths.
///
/// The guest binary carries the assertions — it is the only side that can see
/// whether the compositor closed the connection it ruled on — so the host's
/// job is to boot it and to insist the count it reports is the whole case
/// list. A guest that skipped cases would otherwise exit 0 having proved
/// nothing.
fn metal_sim_ipc_hostile_peer(boot: &mut Boot) -> Result<(), String> {
    let qemu = &mut boot.qemu;
    let result = qemu.run_test("test_rs_ipc_hostile_peer", Duration::from_secs(120));
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!(
            "ipc_hostile_peer exited {:?}:\n{}",
            result.exit_code, result.stdout
        ));
    }
    let Some(refused) = result
        .stdout
        .split("hostile peer: ")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse::<usize>().ok())
    else {
        return Err(format!(
            "ipc_hostile_peer printed no count:\n{}",
            result.stdout
        ));
    };
    // The guest's own case list, restated here so a case deleted on
    // one side is a red run rather than a quieter test.
    const CASES: usize = 3;
    if refused != CASES {
        return Err(format!(
            "the compositor refused {refused} malformed frames, not {CASES}:\n{}",
            result.stdout
        ));
    }
    eprintln!("  [metal-sim] {refused} malformed frames refused, compositor still serving");
    Ok(())
}

/// A client that stops talking, stops listening, or never stops.
///
/// The guest carries the "is it still answering" half; the host carries the half
/// the guest cannot see — whether the desktop is still *painting*, and whether
/// every client the compositor got rid of was named.
///
/// Last in its group: it is the one that abuses the compositor hardest, and
/// its own final assertion is that the desktop is still compositing after it.
fn metal_sim_compositor_stall(boot: &mut Boot) -> Result<(), String> {
    let qemu = &mut boot.qemu;
    let result = qemu.run_test("test_rs_compositor_stall", Duration::from_secs(240));
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!(
            "compositor_stall exited {:?}:\n{}",
            result.exit_code, result.stdout
        ));
    }
    // The guest's own case list, restated here so a case deleted on
    // one side is a red run rather than a quieter test.
    const CASES: usize = 6;
    if !result
        .stdout
        .contains(&format!("compositor stall: {CASES} stalls survived"))
    {
        return Err(format!(
            "the guest did not report {CASES} survived stalls:\n{}",
            result.stdout
        ));
    }

    let frames = |text: &str| text.matches("compositor: frames=").count();

    // Dropped by name, never silently. Three connections never finish
    // a first frame, and one window stops reading its mail.
    const TIMED_OUT: &str = "it never finished its first message";
    let timed_out = result.stdout.matches(TIMED_OUT).count();
    if timed_out < 3 {
        return Err(format!(
            "three connections went quiet mid-handshake and {timed_out} were named:\n{}",
            result.stdout
        ));
    }
    const NOT_READING: &str = "it is not reading";
    if !result.stdout.contains(NOT_READING) {
        return Err(format!(
            "a window stopped reading and the compositor never said so:\n{}",
            result.stdout
        ));
    }

    // And it is still painting once every stall is behind it, on a
    // capture that starts empty — so this counts frames the compositor
    // produced *after* the last case, not frames it produced before
    // the first. Its reporting interval is 2 s.
    //
    // **Two batches, not twenty seconds.** The count is the verdict; what
    // ended the wait was a flat 20 s of host clock, which at width 12 asks
    // a compositor with a twelfth of the machine for ten intervals' work
    // in one.
    let mut after = String::new();
    if let Err(why) =
        await_guest(qemu, &mut after, "two more frame batches", |seen| frames(seen) >= 2)
    {
        return Err(format!(
            "{why}\nthe compositor reported {} frame batches after the last stall:\n{after}",
            frames(&after)
        ));
    }

    let console = format!("{}\n{after}", result.serial);
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    eprintln!(
        "  [metal-sim] {CASES} stalls survived, {timed_out} handshakes timed out by name, \
         desktop still compositing"
    );
    Ok(())
}

/// A client that dies, or asks for something the kernel refuses on its behalf,
/// must cost the compositor that client and nothing else.
///
/// The guest half runs the cases and probes after each; this half asserts what
/// the guest cannot see — that the desktop is still painting, and that the
/// clients dropped along the way were named. A compositor that panics fails
/// this at the probe, at the frame count and at the console check, which is
/// what it should do.
fn metal_sim_client_death(boot: &mut Boot) -> Result<(), String> {
    let qemu = &mut boot.qemu;
    let result = qemu.run_test("test_rs_compositor_client_death", Duration::from_secs(240));
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!(
            "compositor_client_death exited {:?}:\n{}",
            result.exit_code, result.stdout
        ));
    }
    // The guest's own case list, restated here so a case deleted on one side
    // is a red run rather than a quieter test.
    const CASES: usize = 6;
    if !result
        .stdout
        .contains(&format!("compositor client death: {CASES} deaths survived"))
    {
        return Err(format!(
            "the guest did not report {CASES} survived deaths:\n{}",
            result.stdout
        ));
    }

    // Non-vacuity, and the case that motivated the whole run: the compositor
    // has to have met a request whose creator the kernel no longer knows. The
    // guest orders that by construction — reap, then release the process
    // holding the socket — so a run without this line is a defect and never a
    // lost race.
    //
    // **The line is the compositor serving that request, where it used to be
    // the compositor saying the process had exited.** The grant that killed the
    // desktop named a pid; a buffer is a handle now and the connection is what
    // it travels over, so a reaped creator costs its heir nothing and the
    // refusal this once asserted on cannot happen.
    const VANISHED: &str = "a reaped creator's connection still got a window";
    if !result.stdout.contains(VANISHED) {
        return Err(format!(
            "the compositor never served a request from a reaped creator, so this run says \
             nothing about what replaced the grant:\n{}",
            result.stdout
        ));
    }

    const OVERSIZE: &str = "compositor: refusing an inline payload past";
    if !result.stdout.contains(OVERSIZE) {
        return Err(format!(
            "an over-long inline clipboard was not refused by name, so nothing here separates \
             a refusal from a truncation:\n{}",
            result.stdout
        ));
    }
    // A copy's length sizes the region the compositor makes, so a length past
    // the clipboard is refused before any region exists.
    const LONG_COPY: &str = "compositor: refusing a copy of 4294967295 bytes";
    if !result.stdout.contains(LONG_COPY) {
        return Err(format!(
            "a copy longer than any clipboard was not refused by name:\n{}",
            result.stdout
        ));
    }

    // Still painting once every case is behind it, on a capture that starts
    // empty — so this counts frames produced *after* the last case. The
    // compositor's reporting interval is 2 s.
    // The count is the verdict and the wait is a guard, as in
    // `metal_sim_compositor_stall`.
    let frames = |text: &str| text.matches("compositor: frames=").count();
    let mut after = String::new();
    if let Err(why) =
        await_guest(qemu, &mut after, "two more frame batches", |seen| frames(seen) >= 2)
    {
        return Err(format!(
            "{why}\nthe compositor reported {} frame batches after the last client died:\n{after}",
            frames(&after)
        ));
    }

    let console = format!("{}\n{after}", result.serial);
    serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
    eprintln!(
        "  [metal-sim] {CASES} client deaths survived, a reaped creator's request served \
         anyway, desktop still compositing"
    );
    Ok(())
}

/// Run one machine-shape test. Like `run_screen_test`, each of these owns its
/// QEMU — the machine shape *is* the test — except for the runs of adjacent
/// names that share one through `held` (see [`group_boot`]).
fn run_machine_test(
    name: &str,
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    held: &mut Grouped,
) -> Result<(), String> {
    // **Only one QEMU may be up at a time in this process.** Every instance
    // shares one QMP socket path and one `test-bootable.img` under the pid's
    // temp dir, so a guest still running when the next one starts takes that
    // one's socket and it exits before its first line — which is what every
    // test after a group reported the first time a group outlived its members.
    // (It is also what parallel boots in this process would have to fix
    // first.)
    if group_of(name) != held.as_ref().map(|up| up.group) {
        *held = None;
    }
    match name {
        // Body in `tests/common/storage.rs`, so the hunk in this shared file
        // stays one line.
        "foreign_disk_untouched" => storage::foreign_disk_untouched(test_config, c_bins, rust_bins),
        "internal_disk_boot" => storage::internal_disk_boot(test_config, c_bins, rust_bins),
        "partition_claim" => partclaim::partition_claim(test_config, c_bins, rust_bins),
        "partition_claim_gives_up" => {
            partclaim::partition_claim_gives_up(test_config, c_bins, rust_bins)
        }
        "partition_claim_departure" => {
            partclaim::partition_claim_departure(test_config, c_bins, rust_bins)
        }
        "block_duplicate_id" => storage::block_duplicate_id(test_config, c_bins, rust_bins),
        "page_cache_partition_offset" => {
            storage::page_cache_partition_offset(test_config, c_bins, rust_bins)
        }
        "volume_from_another_disk" => {
            storage::volume_from_another_disk(test_config, c_bins, rust_bins)
        }
        "broken_data_volume_is_absent" => {
            storage::broken_data_volume_is_absent(test_config, c_bins, rust_bins)
        }
        "data_candidate_with_bad_geometry_is_absent" => {
            storage::data_candidate_with_bad_geometry_is_absent(test_config, c_bins, rust_bins)
        }
        "home_overwrite_reads_back" => {
            storage::home_overwrite_reads_back(test_config, c_bins, rust_bins)
        }
        "apps_and_home_are_one_filesystem" => {
            storage::apps_and_home_are_one_filesystem(test_config, c_bins, rust_bins)
        }
        "pkg_install_gbae" => pkg::pkg_install_gbae(test_config, c_bins, rust_bins),
        // Body in `tests/common/gpt.rs`, same reason.
        "boot_partition_identity" => common::gpt::boot_partition_identity(test_config, c_bins, rust_bins),
        "machine_reboot" => power::machine_reboot(test_config, c_bins, rust_bins),
        "metal_job_reboot" => power::metal_job_reboot(test_config, c_bins, rust_bins),
        "metal_device_probe" => devices::metal_device_probe(test_config, c_bins, rust_bins),
        "job_deadline_reboots" => power::job_deadline_reboots(test_config, c_bins, rust_bins),
        "quiesce_stops_the_machine" => power::quiesce_stops_the_machine(test_config, c_bins, rust_bins),
        "quiesce_refuses_a_second_shutdown" => power::quiesce_refuses_a_second_shutdown(test_config, c_bins, rust_bins),
        "quiesce_wakes_on_the_last_park" => power::quiesce_wakes_on_the_last_park(test_config, c_bins, rust_bins),
        "quiesce_wakes_on_the_last_teardown" => power::quiesce_wakes_on_the_last_teardown(test_config, c_bins, rust_bins),
        "watchdog_resets" => power::watchdog_resets(test_config, c_bins, rust_bins),
        "loader_watchdog_arms" => power::loader_watchdog_arms(test_config, c_bins, rust_bins),
        "panic_reboots" => power::panic_reboots(test_config, c_bins, rust_bins),
        "panic_before_peripherals_reboots" => {
            power::panic_before_peripherals_reboots(test_config, c_bins, rust_bins)
        }
        "blackbox_panic_chain" => power::blackbox_panic_chain(test_config, c_bins, rust_bins),
        "panic_outlives_the_deadline" => {
            power::panic_outlives_the_deadline(test_config, c_bins, rust_bins)
        }
        "blackbox_done_chain" => power::blackbox_done_chain(test_config, c_bins, rust_bins),
        "boot_deadline_ends_a_wedge" => {
            power::boot_deadline_ends_a_wedge(test_config, c_bins, rust_bins)
        }
        "usb_reset_records_the_phase_it_cut" => {
            power::usb_reset_records_the_phase_it_cut(test_config, c_bins, rust_bins)
        }
        "hard_lockup_ends_a_deaf_cpu" => {
            power::hard_lockup_ends_a_deaf_cpu(test_config, c_bins, rust_bins)
        }
        "usb_reset_hands_devices_back" => {
            power::usb_reset_hands_devices_back(test_config, c_bins, rust_bins)
        }
        "blackbox_foreign_record" => {
            power::blackbox_foreign_record(test_config, c_bins, rust_bins)
        }
        "hang_bounded_by_the_stick" => {
            power::hang_bounded_by_the_stick(test_config, c_bins, rust_bins)
        }
        "blackbox_unclaimed_page" => {
            power::blackbox_unclaimed_page(test_config, c_bins, rust_bins)
        }
        "blackbox_early_panic_sealed" => {
            power::blackbox_early_panic_sealed(test_config, c_bins, rust_bins)
        }
        "blackbox_early_panic_sealed_muted" => {
            power::blackbox_early_panic_sealed_muted(test_config, c_bins, rust_bins)
        }
        "blackbox_fault_sealed" => power::blackbox_fault_sealed(test_config, c_bins, rust_bins),
        // Bodies in `tests/common/usb.rs`, for the same reason.
        "usb_storage_gate" => usb::usb_storage_gate(test_config, c_bins, rust_bins),
        "usb_storage_shapes" => usb::usb_storage_shapes(test_config, c_bins, rust_bins),
        "usb_boot_stick_pulled" => usb::usb_boot_stick_pulled(test_config, c_bins, rust_bins),
        "usb_refused_disk_first" => {
            usb::usb_refused_disk_first(test_config, c_bins, rust_bins)
        }
        "xhci_scan_hands_over_a_free_slot" => {
            usb::xhci_scan_hands_over_a_free_slot(test_config, c_bins, rust_bins)
        }
        "usb_pool_exhausted" => usb::usb_pool_exhausted(test_config, c_bins, rust_bins),
        "usb_short_read" => usb::usb_short_read(test_config, c_bins, rust_bins),
        // Body in `tests/common/volumes.rs`, same reason.
        "esp_filesystem" => common::volumes::esp_filesystem(test_config, c_bins, rust_bins),
        "log_flush_retry" => common::volumes::log_flush_retry(test_config, c_bins, rust_bins),
        // Body in `tests/common/toybox.rs`, same reason.
        "toybox_cp_volume" => common::toybox::cp_volume(test_config, c_bins, rust_bins),
        "kernel_log_file" => common::volumes::kernel_log_file(test_config, c_bins, rust_bins),
        // Body in `tests/common/volumes.rs`, same reason: the host-side oracle
        // shuts the guest down and reads `/log` back with `toyos-fat32-check`.
        "writeback_durability" => common::volumes::writeback_durability(test_config, c_bins, rust_bins),
        // Same again: the FAT32 read side's revocation, judged off the volume the
        // guest's unlink-and-reallocate cycle left behind.
        "fat_backing_revoked" => common::volumes::fat_backing_revoked(test_config, c_bins, rust_bins),
        // `sysret-ss-probe` has iod null SS, force a switch, and log whether the
        // switch reloaded it; a missing `mov ss` turns `reloaded` into `NOT`.
        "sysret_ss_reload" => {
            let options = BootOptions {
                kernel_params: &["sysret-ss-probe"],
                ..Default::default()
            };
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            // iod's probe reports on either side of the ready marker and the
            // drain reads only lines after it, so the boot log is asked first;
            // the drain's ceiling is a liveness bound on a report still owed.
            let mut log = qemu.boot_log().to_string();
            if !log.lines().any(sysret_ss_reported) {
                log += &qemu.drain_until(Duration::from_secs(10), sysret_ss_reported);
            }
            sysret_ss(&log)
        }
        "fsync_failed_commit" => common::volumes::fsync_failed_commit(test_config, c_bins, rust_bins),
        "redirty_mid_flush" => common::volumes::redirty_mid_flush(test_config, c_bins, rust_bins),
        "ftruncate_flush_race" => common::volumes::ftruncate_flush_race(test_config, c_bins, rust_bins),
        "fs_rename_durable" => common::volumes::fs_rename_durable(test_config, c_bins, rust_bins),
        "fs_dirs_durable" => common::volumes::fs_dirs_durable(test_config, c_bins, rust_bins),
        "quiesce_leaves_the_volume_whole" => common::volumes::quiesce_leaves_the_volume_whole(test_config, c_bins, rust_bins),
        // The lost-wake canary with the window it guards held open: every pipe
        // wait reads its condition, waits for a post to land, then parks, so
        // the ping-pong's posts land between the two. A commit that ignored the
        // notified bit parks for good, and the run's ceiling reds it.
        "blocking_read_window" => {
            let options = BootOptions {
                kernel_params: &["watch-window"],
                ..Default::default()
            };
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();
            serial::Serial::named("boot console", boot.as_str()).must_be_clean()?;
            let result = qemu.run_test("test_rs_blocking_read_stress", Duration::from_secs(30));
            if !check_rust_result(&result) {
                return Err(format!(
                    "blocking_read_window failed:\n{}\nkernel log while it ran:\n{}{}",
                    result.stdout, result.before, result.serial
                ));
            }
            Ok(())
        }
        // Two CPUs: the held copy spins in the kernel while its sibling unmaps
        // and maps on the other.
        "user_copy_races_munmap" => {
            let options = BootOptions {
                smp: 2,
                kernel_params: &["copy-meets-a-remap"],
                ..Default::default()
            };
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let result = qemu.run_test("test_rs_copy_out_races_munmap", Duration::from_secs(30));
            if !check_rust_result(&result) {
                return Err(format!(
                    "user_copy_races_munmap failed:\n{}\nkernel log while it ran:\n{}{}",
                    result.stdout, result.before, result.serial
                ));
            }
            Ok(())
        }
        // Two CPUs: the held spawn spins in the kernel while its sibling stores
        // on the other.
        "tls_rebase_window" => {
            let options = BootOptions {
                smp: 2,
                kernel_params: &["tls-rebase-window"],
                ..Default::default()
            };
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let result = qemu.run_test("test_rs_tls_dtv_race", Duration::from_secs(30));
            let log = format!("{}{}", result.before, result.serial);
            let unreachable = log.lines().filter(|l| l.contains(TLS_BLOCK_UNREACHABLE)).count();
            if !check_rust_result(&result) || unreachable != TLS_RACE_WATCHED {
                return Err(format!(
                    "tls_rebase_window failed: {unreachable} of {TLS_RACE_WATCHED} watched blocks \
                     unreachable before their rebase:\n{}\nkernel log while it ran:\n{log}",
                    result.stdout
                ));
            }
            Ok(())
        }
        // The write-back queue's re-open control: `writeback-stall` parks `iod`
        // before it drains, so the guest can prove a re-open before the flush
        // reads the pinned pages and not the NVMe `/home` device.
        "writeback_reopen" => {
            let options = BootOptions {
                kernel_params: &["writeback-stall"],
                ..Default::default()
            };
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();
            serial::Serial::named("boot console", boot.as_str()).must_be_clean()?;
            let result = qemu.run_test("test_rs_writeback_reopen", Duration::from_secs(30));
            if !check_rust_result(&result) {
                return Err(format!(
                    "writeback_reopen failed:\n{}\nkernel log while it ran:\n{}{}",
                    result.stdout, result.before, result.serial
                ));
            }
            Ok(())
        }
        // The other half of the same stall, on the path the file cache does not
        // answer: a spawn reads a *device* view (`Vfs::open_backing`), so a
        // binary written and closed with the write-back still owed used to load
        // as `ELF: fewer bytes than a file header`. Same actuator, and the same
        // reason it needs its own boot.
        "writeback_spawn" => {
            let options = BootOptions {
                kernel_params: &["writeback-stall"],
                ..Default::default()
            };
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();
            serial::Serial::named("boot console", boot.as_str()).must_be_clean()?;
            let result = qemu.run_test("test_rs_writeback_spawn", Duration::from_secs(30));
            if !check_rust_result(&result) {
                return Err(format!(
                    "writeback_spawn failed:\n{}\nkernel log while it ran:\n{}{}",
                    result.stdout, result.before, result.serial
                ));
            }
            Ok(())
        }
        "kernel_heartbeat" => {
            // The instrument for a machine whose log cannot say whether it was
            // alive: ten of the owner's boots are byte-identical between the
            // ones that froze and the ones that did not. What a guest can prove
            // of it is that the lines *keep coming*, that each carries the
            // pin's state beside it, and that the pin's state is read off the
            // chip.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/metalcase");
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                smp: 8,
                kernel_params: &["heartbeat"],
                ..Default::default()
            };
            // **A heartbeat and the `i8042: line` under it are one reading**,
            // and `heartbeat::poll` emits them as two `log!`s — so a capture can
            // end between them. Counting the two kinds against each other reads
            // that as a pin whose state was unreadable, which is the one thing
            // this pairing exists to detect. So the unit is the pair, and a beat
            // with nothing after it at all is a reading this capture does not
            // hold.
            fn whole(log: &str) -> (Vec<&str>, Vec<usize>) {
                let captured: Vec<&str> = log.lines().collect();
                let at: Vec<usize> = captured
                    .iter()
                    .enumerate()
                    .filter(|(_, l)| l.contains("heartbeat: t="))
                    .map(|(i, _)| i)
                    .collect();
                let torn = at.last().is_some_and(|&i| i + 1 == captured.len());
                let kept = captured.len() - usize::from(torn);
                (captured[..kept].to_vec(), at[..at.len() - usize::from(torn)].to_vec())
            }
            /// Whole beats the verdict is read from.
            const BEATS: usize = 5;

            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
            let mut log = qemu.boot_log().to_string();
            // The instrument has to keep reporting: a guest that stops is what
            // the harness ceiling reds.
            if let Err(why) =
                await_guest(&mut qemu, &mut log, "five whole heartbeats", |c| whole(c).1.len() >= BEATS)
            {
                return Err(format!("{why}\n{log}"));
            }
            let (captured, at) = whole(&log);
            let beats: Vec<&str> = at.iter().map(|&i| captured[i]).collect();
            // Each pair, positionally: `report_line` is the statement after the
            // heartbeat's `log!`, and another CPU's line may land between the
            // two commits, so what is asserted is one pin reading before the
            // next beat rather than the line immediately below.
            let pin_in = |from: usize, to: usize| {
                captured[from..to].iter().filter(|l| l.contains("i8042: line ")).count()
            };
            let unpaired: Vec<String> = at
                .iter()
                .enumerate()
                .filter(|&(n, &i)| pin_in(i + 1, at.get(n + 1).copied().unwrap_or(captured.len())) != 1)
                .map(|(_, &i)| captured[i].to_string())
                .collect();
            if !unpaired.is_empty() {
                return Err(format!(
                    "{} of {} heartbeats carry no `i8042: line` of their own — the pin's state is \
                     what separates a machine with nothing to do from one whose input died, and it \
                     has to be readable at every heartbeat or the pairing is a guess\n{}\n{log}",
                    unpaired.len(),
                    beats.len(),
                    unpaired.iter().take(4).cloned().collect::<Vec<_>>().join("\n"),
                ));
            }
            // And the clock in the line advances, or the timestamp cannot
            // localise a death.
            let stamps: Vec<&str> = beats
                .iter()
                .filter_map(|l| l.split("heartbeat: t=").nth(1))
                .map(|s| s.split_whitespace().next().unwrap_or(""))
                .collect();
            if stamps.first() == stamps.last() {
                return Err(format!(
                    "every heartbeat carries the same timestamp {:?}\n{log}",
                    stamps.first()
                ));
            }
            // The line beside every heartbeat, and the reason it is there: the
            // T14's freeze reads `alive=8/8 ran=0` under both live hypotheses,
            // and only the pin's own state separates them. What has teeth here
            // is not that the line exists but that its vector is *read back off
            // the chip* and matches the one `init` said it programmed — a probe
            // printing a literal, or reading the wrong register, disagrees.
            let armed = log
                .lines()
                .find_map(|l| l.split("scanning on, GSI ").nth(1))
                .map(|s| s.to_string());
            let Some(armed) = armed else {
                return Err(format!(
                    "no i8042 arming line on a Profile::Metal guest, so nothing says which GSI or \
                     vector the probe below should have read\n{log}"
                ));
            };
            // `1 -> vec 0x24 apic 0 on`
            let mut fields = armed.split_whitespace();
            let kbd_gsi = fields.next().unwrap_or("").to_string();
            let vector = fields.nth(2).unwrap_or("").trim_start_matches("0x").to_string();
            let lines: Vec<&str> = log.lines().filter(|l| l.contains("i8042: line ")).collect();
            // `rte=0x0000000000000024`: the vector is the low byte, bit 16 is
            // the mask. Both are the chip's answer, not ours.
            let healthy = |l: &str| {
                l.split("rte=0x")
                    .skip(1)
                    .map(|e| e.split_whitespace().next().unwrap_or(""))
                    .all(|e| {
                        u64::from_str_radix(e, 16).is_ok_and(|entry| {
                            entry & 0xFF == u64::from_str_radix(&vector, 16).unwrap_or(0)
                                && entry & (1 << 16) == 0
                        })
                    })
            };
            let wrong: Vec<&&str> = lines.iter().filter(|l| !healthy(l)).collect();
            if !wrong.is_empty() || !lines.iter().all(|l| l.contains(&format!("kbd gsi={kbd_gsi} "))) {
                return Err(format!(
                    "{} of {} `i8042: line` readings carry an entry that is masked, or whose vector \
                     is not the {vector} `init` programmed on GSI {kbd_gsi} — the probe is not \
                     reading the chip\n{}\n{log}",
                    wrong.len(),
                    lines.len(),
                    wrong.iter().take(4).map(|l| l.to_string()).collect::<Vec<_>>().join("\n"),
                ));
            }
            // OBF stuck is the state the probe exists to name, so a healthy
            // guest must never show it — otherwise the reading is noise and a
            // metal log carrying it proves nothing.
            let obf: Vec<&&str> = lines
                .iter()
                .filter(|l| {
                    l.split("status=0x")
                        .nth(1)
                        .and_then(|s| u8::from_str_radix(s.split_whitespace().next()?, 16).ok())
                        .is_some_and(|s| s & 1 != 0)
                })
                .collect();
            if !obf.is_empty() {
                return Err(format!(
                    "{} of {} `i8042: line` readings found the output buffer full on a guest whose \
                     input is healthy — a set bit there is meant to mean the controller is holding \
                     a byte no ISR will ever read\n{}\n{log}",
                    obf.len(),
                    lines.len(),
                    obf.iter().take(4).map(|l| l.to_string()).collect::<Vec<_>>().join("\n"),
                ));
            }
            eprintln!(
                "  [heartbeat] {} whole lines, each with its own pin reading, t={} → t={}; {} \
                 i8042 line reading(s), vec 0x{vector} on gsi {kbd_gsi}, none masked, none with \
                 OBF set",
                beats.len(),
                stamps.first().unwrap_or(&"?"),
                stamps.last().unwrap_or(&"?"),
                lines.len(),
            );
            Ok(())
        }
        "wall_clock_rtc_dead" => common::wallclock::undated(
            test_config,
            c_bins,
            rust_bins,
            "wall-clock-dead.img",
            &["rtc-dead"],
            "its update flag never cleared",
        ),
        "wall_clock_rtc_unstable" => common::wallclock::undated(
            test_config,
            c_bins,
            rust_bins,
            "wall-clock-unstable.img",
            &["rtc-unstable"],
            "no two of 4 reads agreed",
        ),
        "wall_clock_no_century" => common::wallclock::no_century(test_config, c_bins, rust_bins),
        "wall_clock_century_register" => {
            common::wallclock::century_from_the_register(test_config, c_bins, rust_bins)
        }
        "wall_clock_utc" => common::wallclock::rtc_is_utc(test_config, c_bins, rust_bins),
        "file_mtime_survives_a_reboot" => {
            common::wallclock::file_mtime_survives_a_reboot(test_config, c_bins, rust_bins)
        }
        "file_mtime_undated" => common::wallclock::file_mtime_undated(test_config, c_bins, rust_bins),
        "late_storage_connect" => common::volumes::late_storage_connect(test_config, c_bins, rust_bins),
        "root_candidate_malformed" => {
            common::volumes::root_candidate_malformed(test_config, c_bins, rust_bins)
        }
        "root_named_but_absent" => {
            common::volumes::root_named_but_absent(test_config, c_bins, rust_bins)
        }
        "root_chunk_refused" => common::volumes::root_chunk_refused(test_config, c_bins, rust_bins),
        "root_chunk_refused_on_a_usb_stick" => {
            common::volumes::root_chunk_refused_on_a_usb_stick(test_config, c_bins, rust_bins)
        }
        "root_candidate_overlaps" => common::volumes::root_candidate_overlaps(test_config, c_bins, rust_bins),
        "root_named_twice_on_the_boot_disk" => {
            common::volumes::root_named_twice_on_the_boot_disk(test_config, c_bins, rust_bins)
        }
        "root_named_twice" => {
            common::volumes::root_named_twice(test_config, c_bins, rust_bins)
        }
        "log_partition_layout" => {
            common::volumes::log_partition_layout(test_config, c_bins, rust_bins)
        }
        "log_partition_identity" => {
            common::volumes::log_partition_identity(test_config, c_bins, rust_bins)
        }
        "log_backing_read_error" => {
            common::volumes::log_backing_read_error(test_config, c_bins, rust_bins)
        }
        "boot_volume_metadata_error" => {
            common::volumes::boot_volume_metadata_error(test_config, c_bins, rust_bins)
        }
        "usb_storage_write_error" => usb::usb_storage_write_error(test_config, c_bins, rust_bins),
        "usb_flush_optional" => usb::usb_flush_optional(test_config, c_bins, rust_bins),
        "xhci_deaf_registers" => usb::xhci_deaf_registers(test_config, c_bins, rust_bins),
        "xhci_slow_connect" => usb::xhci_slow_connect(test_config, c_bins, rust_bins),
        "xhci_portsc_rw1c" => usb::xhci_portsc_rw1c(test_config, c_bins, rust_bins),
        "usb_transport_break" => usb::usb_transport_break(test_config, c_bins, rust_bins),
        "xhci_full_speed_device" => {
            usb::xhci_full_speed_device(test_config, c_bins, rust_bins)
        }
        "xhci_superspeed_ports" => usb::xhci_superspeed_ports(test_config, c_bins, rust_bins),
        "xhci_flap" => usb::xhci_flap(test_config, c_bins, rust_bins),
        // Body in `tests/common/iommu.rs`, same reason.
        "iommu_discovery" => common::iommu::iommu_discovery(test_config, c_bins, rust_bins),
        // Body in `tests/common/logread.rs`, so the hunk here stays one line.
        "log_conservation_smp2" => {
            common::logread::log_conservation_smp2(test_config, c_bins, rust_bins)
        }
        "log_nested_emit" => common::logread::log_nested_emit(test_config, c_bins, rust_bins),
        "log_reserve_window" => {
            common::logread::log_reserve_window(test_config, c_bins, rust_bins)
        }
        "log_reserve_window_negative" => {
            common::logread::log_reserve_window_negative(test_config, c_bins, rust_bins)
        }
        "log_poll_outlives_a_close" => {
            common::logread::log_poll_outlives_a_close(test_config, c_bins, rust_bins)
        }
        // Body in `tests/common/console.rs`, same reason.
        "c_capture_ignores_daemon_lines" => {
            common::console::c_capture_ignores_daemon_lines(test_config, c_bins, rust_bins)
        }
        "keyboard_claim_close_spares_stdin" => {
            common::console::keyboard_claim_close_spares_stdin(test_config, c_bins, rust_bins)
        }
        "iommu_context_absent" => common::iommu::iommu_context_absent(test_config, c_bins, rust_bins),
        "iommu_empty_domain" => common::iommu::iommu_empty_domain(test_config, c_bins, rust_bins),
        "iommu_interrupt_remapping" => {
            common::iommu::iommu_interrupt_remapping(test_config, c_bins, rust_bins)
        }
        "iommu_virtio_platform" => {
            common::iommu::iommu_virtio_platform(test_config, c_bins, rust_bins)
        }
        "iommu_domain_isolation" => {
            common::iommu::iommu_domain_isolation(test_config, c_bins, rust_bins)
        }
        "iommu_gpu_scanout_swap" => {
            common::iommu::iommu_gpu_scanout_swap(test_config, c_bins, rust_bins)
        }
        "iommu_gpu_foreign_backing" => {
            common::iommu::iommu_gpu_foreign_backing(test_config, c_bins, rust_bins)
        }
        "iommu_hda_foreign_bdl" => {
            common::iommu::iommu_hda_foreign_bdl(test_config, c_bins, rust_bins)
        }
        "iommu_sound_foreign_dma" => {
            common::iommu::iommu_sound_foreign_dma(test_config, c_bins, rust_bins)
        }
        "userdev_dma_fault" => {
            common::iommu::userdev_dma_fault(test_config, c_bins, rust_bins)
        }
        "userdev_residue_is_its_own" => {
            common::iommu::userdev_residue_is_its_own(test_config, c_bins, rust_bins)
        }
        // Bodies in `tests/common/blockd.rs`.
        "blockd_serves_partitions" => {
            common::blockd::blockd_serves_partitions(test_config, c_bins, rust_bins)
        }
        "blockd_survives_its_death" => {
            common::blockd::blockd_survives_its_death(test_config, c_bins, rust_bins)
        }
        "blockd_dma_outside_the_lent" => {
            common::blockd::blockd_dma_outside_the_lent(test_config, c_bins, rust_bins)
        }
        "blockd_lends_within_its_bound" => {
            common::blockd::blockd_lends_within_its_bound(test_config, c_bins, rust_bins)
        }
        "double_fault_stack" => faults::double_fault_stack(test_config, c_bins, rust_bins),
        "syscall_window_nmi" => faults::syscall_window_nmi(test_config, c_bins, rust_bins),
        "syscall_window_nmi_controls" => {
            faults::syscall_window_nmi_controls(test_config, c_bins, rust_bins)
        }
        "idle_stack_guard" => faults::idle_stack_guard(test_config, c_bins, rust_bins),
        "dump_left_pending_is_owed" => {
            faults::dump_left_pending_is_owed(test_config, c_bins, rust_bins)
        }
        "diskless_boot" => faults::diskless_boot(test_config, c_bins, rust_bins),
        "virtio_net_no_msix" => faults::virtio_net_no_msix(),
        "pci_claim_caps_truncated" => faults::claim_caps_truncated(),
        // Body in `tests/common/clang.rs`.
        "c_hello" => common::clang::c_hello(rust_bins),
        "doom_frames" => doom_frames(rust_bins),
        "metal_sim_compositor" => {
            metal_sim_compositor(group_boot(held, METAL_SIM_DESKTOP, || {
                boot_metal_sim_desktop(rust_bins)
            }))
        }
        "metal_sim_scanout_wc" => {
            metal_sim_scanout_wc(group_boot(held, METAL_SIM_DESKTOP, || {
                boot_metal_sim_desktop(rust_bins)
            }))
        }
        "metal_sim_window_caps" => {
            metal_sim_window_caps(group_boot(held, METAL_SIM_DESKTOP, || {
                boot_metal_sim_desktop(rust_bins)
            }))
        }
        "metal_sim_ipc_hostile_peer" => {
            metal_sim_ipc_hostile_peer(group_boot(held, METAL_SIM_DESKTOP, || {
                boot_metal_sim_desktop(rust_bins)
            }))
        }
        "metal_sim_compositor_stall" => {
            metal_sim_compositor_stall(group_boot(held, METAL_SIM_DESKTOP, || {
                boot_metal_sim_desktop(rust_bins)
            }))
        }
        "metal_sim_client_death" => {
            metal_sim_client_death(group_boot(held, METAL_SIM_DESKTOP, || {
                boot_metal_sim_desktop(rust_bins)
            }))
        }
        "i8042_no_spurious_wake" => i8042_no_spurious_wake(group_boot(held, I8042_TRACE, || {
            boot_i8042_trace(test_config, c_bins, rust_bins)
        })),
        "i8042_mouse" => i8042_mouse(group_boot(held, I8042_TRACE, || {
            boot_i8042_trace(test_config, c_bins, rust_bins)
        })),
        "swiss_german_layout" => {
            swiss_german_layout(&mut boot_locale(test_config, c_bins, rust_bins))
        }
        "locale_detect" => {
            let boot = group_boot(held, LOCALE_WIZARD, || {
                boot_locale(test_config, c_bins, rust_bins)
            });
            locale_detect(&mut boot.qemu)
        }
        "locale_detect_unrecognized" => {
            let boot = group_boot(held, LOCALE_WIZARD, || {
                boot_locale(test_config, c_bins, rust_bins)
            });
            locale_detect_unrecognized(&mut boot.qemu)
        }
        // The three judges of `tests/common/ssh.rs`, on one `tests/sshdcase`
        // boot. `test_config` is not theirs: the key staged and the forward
        // opened are that config's.
        "sshd_exec" => {
            let boot = group_boot(held, SSHD_LOGIN, || common::ssh::boot(rust_bins));
            common::ssh::exec_gate(&mut boot.qemu)
        }
        "sshd_files" => {
            let boot = group_boot(held, SSHD_LOGIN, || common::ssh::boot(rust_bins));
            common::ssh::files_gate(&mut boot.qemu)
        }
        "sshd_key_auth" => {
            let boot = group_boot(held, SSHD_LOGIN, || common::ssh::boot(rust_bins));
            common::ssh::key_auth_gate(&mut boot.qemu)
        }
        "console_locale_detect" => console_locale_detect(),
        "desktop_locale_detect" => desktop_locale_detect(),
        "desktop_typing_damage" => desktop_typing_damage(),
        "desktop_window_child" => desktop_window_child(rust_bins),
        "toolkit_iced" => toolkit_iced(),
        "toolkit_window_wake" => toolkit_window_wake(rust_bins),
        "toolkit_winit_loop" => toolkit_winit_loop(rust_bins),
        "toolkit_winit_pace" => toolkit_winit_pace(rust_bins),
        "blocked_dump" => blocked_dump(),
        "xhci_many_devices" => {
            // The T14's internal controller carries a camera, Bluetooth and a
            // fingerprint reader next to the boot stick, and every profile in
            // this tree had at most three devices on the bus — so no test
            // could see a driver that stopped at three, and no test could see
            // two devices of one class landing on one interrupt ring.
            let options = BootOptions {
                profile: qemu::Profile::MetalUsb,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            let usb = usb_argv(&argv);
            // The profile's claim is about the bus, so it is checked against
            // argv: a console line cannot distinguish "the driver bound one
            // keyboard" from "only one keyboard was ever attached".
            if usb.len() < 4 {
                return Err(format!("this profile needs more USB devices than {usb:?}"));
            }
            if usb.iter().filter(|d| d.starts_with("usb-kbd")).count() < 2 {
                return Err(format!("two keyboards are the point; argv has {usb:?}"));
            }
            if !usb.iter().any(|d| d.starts_with("usb-storage")) {
                return Err(format!("no non-HID device on the bus: {usb:?}"));
            }

            let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let log = qemu.boot_log().to_string();

            // Where the block count came from, which is the thing this work
            // exists to protect and the thing no count of devices or rings can
            // see: a fixed cap of any value at or above the size of this bus
            // leaves every other assertion here green.
            let Some(dma) = parse_xhci_layout(&log) else {
                return Err(format!("the driver printed no DMA layout line:\n{log}"));
            };
            let room = dma.pool_kib * 1024 / dma.stride;
            if room <= dma.blocks {
                return Err(format!(
                    "the pool holds {room} blocks of {} B and the driver claimed {}: {dma:?}",
                    dma.stride, dma.blocks
                ));
            }
            // The pool has room for four times what this controller can
            // address, so the slot count is the binding term of the two and
            // the block count has to be it exactly. (A cap that happened to
            // equal 64 would still pass — no QEMU controller can tell those
            // apart. Every other constant cannot.)
            if dma.blocks != dma.cap_slots {
                return Err(format!(
                    "device blocks={} with max_slots={} and room for {room} — the block count \
                     is not the controller's slot count:\n{log}",
                    dma.blocks, dma.cap_slots
                ));
            }
            // And it fit in the single 2 MiB page DmaPool was going to hand
            // out for the head regardless, which is the whole cost argument.
            if dma.pool_kib != 2048 {
                return Err(format!(
                    "the pool is {} KiB, not the one 2 MiB page the head already forces: {dma:?}",
                    dma.pool_kib
                ));
            }

            // One slot per device on the bus, non-HID included: the driver
            // enables a slot before it can know what the device is.
            let slots = parse_xhci_slots(&log);
            if slots.len() != usb.len() {
                return Err(format!(
                    "{} devices on the bus, {} slots enabled ({slots:?}):\n{log}",
                    usb.len(),
                    slots.len()
                ));
            }
            let mut distinct = slots.clone();
            distinct.sort_unstable();
            distinct.dedup();
            if distinct.len() != slots.len() {
                return Err(format!("a slot id came back twice: {slots:?}"));
            }

            // Each HID on its own interrupt ring and its own report buffer.
            // Two keyboards sharing a ring is the defect this asserts against,
            // and it is silent from every other angle.
            let binds = parse_xhci_binds(&log);
            let keyboards = binds.iter().filter(|b| b.kind == "keyboard").count();
            if keyboards != 2 {
                return Err(format!("{keyboards} keyboards bound, want 2: {binds:?}\n{log}"));
            }
            if binds.len() < 4 {
                return Err(format!("only {} HID devices bound: {binds:?}\n{log}", binds.len()));
            }
            let mut rings: Vec<usize> = binds.iter().map(|b| b.int_ring).collect();
            rings.sort_unstable();
            rings.dedup();
            if rings.len() != binds.len() {
                return Err(format!(
                    "{} devices share {} interrupt rings: {binds:?}",
                    binds.len(),
                    rings.len()
                ));
            }
            // And every device on the bus is accounted for exactly once: the
            // HIDs bound above, the boot stick bound as a disk, the hub walked
            // past. An inequality here would let a driver that bound the stick
            // *and* skipped it, or that stopped enumerating early, pass.
            let disks = log.matches("usb-storage: disk ").count();
            let skipped = log.matches("no HID boot interface found").count();
            if binds.len() + disks + skipped != usb.len() {
                return Err(format!(
                    "{} HID + {disks} disk + {skipped} skipped is not the {} devices on the bus:\n{log}",
                    binds.len(),
                    usb.len()
                ));
            }
            if disks != 1 {
                return Err(format!("{disks} disks bound, want the boot stick:\n{log}"));
            }
            serial::Serial::named("boot console", log.as_str()).must_be_clean()?;
            eprintln!(
                "  [xhci] {} devices, {} slots, {keyboards} keyboards on {} distinct rings, \
                 {disks} disk; {} blocks of {} B for max_slots={}, scratchpad={}, pool {} KiB",
                usb.len(),
                slots.len(),
                rings.len(),
                dma.blocks,
                dma.stride,
                dma.cap_slots,
                dma.scratchpad,
                dma.pool_kib
            );
            Ok(())
        }
        "xhci_second_controller" => {
            // The T14's shape, and the defect that shape found. Tiger Lake has
            // two xHCI controllers — the Thunderbolt block's at 00:0d.0 and the
            // PCH's at 00:14.0, identical in class, subclass and prog_if — and
            // the laptop's own ports hang off the second. The kernel took the
            // first PCI match, so a real boot logged one `xHCI: found at PCI
            // 00:0d.0` and then `no HID devices found` on a machine whose
            // keyboard was one bus over. Every profile in this tree had exactly
            // one controller, so nothing could see it.
            let options = BootOptions {
                profile: qemu::Profile::MetalXhciSecond,
                qmp: true,
                // Nothing else on this machine may be able to deliver a
                // keystroke. With the i8042 on, a kernel that never found the
                // second controller could still be handed the key by QEMU's
                // PS/2 keyboard and everything below would pass with the defect
                // intact.
                i8042: false,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            let controllers = xhci_argv(&argv);
            if controllers.len() != 2 {
                return Err(format!(
                    "this profile is two controllers or it is nothing; argv has {controllers:?}"
                ));
            }
            // And every USB device is on the second of them. This is the
            // assertion that stops the test passing for the wrong reason: a
            // keyboard on the first controller is found by the defect too, and
            // no console line can tell that apart from the fix working.
            let usb = usb_argv(&argv);
            if let Some(bad) = usb.iter().find(|d| !d.contains("bus=xhci1.0")) {
                return Err(format!(
                    "{bad} is not on the second controller — a driver that stops at the \
                     first would find it"
                ));
            }
            for want in ["usb-kbd", "usb-mouse"] {
                if !usb.iter().any(|d| d.starts_with(want)) {
                    return Err(format!("no {want} to find: {usb:?}"));
                }
            }
            if !argv.iter().any(|a| a.contains("i8042=off")) {
                return Err("the i8042 is on; a PS/2 keyboard could deliver instead".to_string());
            }

            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();

            // Both controllers were brought up. One line here is the defect's
            // exact signature on the laptop.
            let found = boot.matches("xHCI: found at PCI ").count();
            if found != 2 {
                return Err(format!("{found} controller(s) initialised, want 2:\n{boot}"));
            }
            let msix = boot.matches("xHCI: MSI-X enabled").count();
            if msix != 2 {
                return Err(format!(
                    "{msix} of the two controllers were armed on MSI-X; one that publishes a \
                     table and takes MSI is the older mechanism chosen where the newer one was \
                     there:\n{boot}"
                ));
            }
            // And the empty one came up rather than being skipped: it has been
            // reset and armed with MSI-X, so dropping it would leave a live
            // interrupter with nothing draining its event ring.
            if !boot.contains("xHCI: no HID devices on the controller") {
                return Err(format!(
                    "the controller with nothing on it never reported itself:\n{boot}"
                ));
            }
            let binds = parse_xhci_binds(&boot);
            for want in ["keyboard", "mouse"] {
                if binds.iter().filter(|b| b.kind == want).count() != 1 {
                    return Err(format!("{want} not bound exactly once: {binds:?}\n{boot}"));
                }
            }
            // The boot stick is on the second controller too, so the disk
            // index the block layer holds names a device the first controller
            // does not have — the flattening `with_disk` does.
            if boot.matches("usb-storage: disk 0 ready").count() != 1 {
                return Err(format!("the stick on the second controller is not disk 0:\n{boot}"));
            }

            // Then the part no log line can show: an injected keystroke and an
            // injected pointer delta reach a userland process. Ground truth is
            // the host's own injection at the device boundary; the assertion is
            // what the guest printed.
            let Some((scale_x, scale_y)) = parse_rel_scale(&boot) else {
                return Err(format!("the kernel never said what pointer scale it used:\n{boot}"));
            };
            const DX: i32 = 40;
            const DY: i32 = -30;
            // Off the origin first: the accumulated position clamps at 0, so a
            // move up or left from there is invisible. A boot mouse reports each
            // axis as an i8, so this arrives clamped and its exact value is not
            // something to assert on.
            let (result, sent) = input_events_run(&mut qemu, (100, 100), (DX, DY));
            if let Some(err) = &result.error {
                return Err(format!("{err} after {sent} of the sequence\n{}", result.stdout));
            }

            let keys = parse_key_events(&result.stdout);
            let typed: String = keys
                .iter()
                .filter(|e| e.modifiers & 0x10 == 0)
                .map(|e| e.translated.as_str())
                .collect();
            if !typed.contains("hello") {
                return Err(format!(
                    "typed {typed:?}, want it to contain \"hello\" — the keyboard on the \
                     second controller never reached userland:\n{}",
                    result.stdout
                ));
            }

            let pointer = parse_mouse_events(&result.stdout);
            // The delta the wire carried, not "it moved": a sign error in dy
            // and a dropped high bit both survive "it moved".
            let want = (DX * scale_x, DY * scale_y);
            let deltas: Vec<(i32, i32)> = pointer
                .windows(2)
                .map(|w| (w[1].x as i32 - w[0].x as i32, w[1].y as i32 - w[0].y as i32))
                .collect();
            if !deltas.contains(&want) {
                return Err(format!(
                    "no pointer event moved by {want:?}; deltas seen: {deltas:?}\n{}",
                    result.stdout
                ));
            }
            let Some(down) = pointer.iter().position(|e| e.buttons == 0x01) else {
                return Err(format!("no left-button-down event; buttons seen: {:?}",
                    pointer.iter().map(|e| e.buttons).collect::<std::collections::BTreeSet<_>>()));
            };
            if !pointer[down + 1..].iter().any(|e| e.buttons == 0x00) {
                return Err(format!("the left button went down and never came up: {pointer:?}"));
            }
            eprintln!(
                "  [xhci] 2 controllers, HID only on the second; {} key events (typed {typed:?}), \
                 {} pointer events, delta {want:?} delivered",
                keys.len(),
                pointer.len()
            );
            Ok(())
        }
        "xhci_two_controllers" => {
            // Composition across controllers. `keyboard::handle_key` and
            // `mouse::handle_motion` are one held-set and one button merge for
            // the whole machine, which was argued for two devices on one bus
            // and never asked about two buses. The pointer half of it was
            // false: the merge was keyed by xHCI slot id, and slot ids are per
            // controller, so a pointer on slot 1 of each of two controllers was
            // one entry and each report published the other's buttons.
            let options = BootOptions {
                profile: qemu::Profile::MetalXhciBoth,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            let controllers = xhci_argv(&argv);
            if controllers.len() != 2 {
                return Err(format!("want two controllers, argv has {controllers:?}"));
            }
            let usb = usb_argv(&argv);
            for bus in ["bus=xhci.0", "bus=xhci1.0"] {
                let pointers = usb
                    .iter()
                    .filter(|d| d.contains(bus) && d.starts_with("usb-mouse"))
                    .count();
                if pointers != 1 {
                    return Err(format!(
                        "{pointers} pointer(s) on {bus}; the collision needs one on each: {usb:?}"
                    ));
                }
                if !usb.iter().any(|d| d.contains(bus) && d.starts_with("usb-kbd")) {
                    return Err(format!("no keyboard on {bus}: {usb:?}"));
                }
            }

            let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();

            let found = boot.matches("xHCI: found at PCI ").count();
            if found != 2 {
                return Err(format!("{found} controller(s) initialised, want 2:\n{boot}"));
            }
            if !boot.contains("xHCI: 2 controller(s), 5 HID device(s)") {
                return Err(format!(
                    "the machine-wide totals are not 2 controllers and 5 HID devices:\n{boot}"
                ));
            }
            let binds = parse_xhci_binds(&boot);
            for (want, count) in [("keyboard", 3), ("mouse", 2)] {
                let got = binds.iter().filter(|b| b.kind == want).count();
                if got != count {
                    return Err(format!("{got} {want}(s) bound, want {count}: {binds:?}\n{boot}"));
                }
            }

            // The merge itself. Two pointers, two entries in the button table,
            // and — the reason this profile is shaped the way it is — the same
            // slot id on both, so a source derived from the slot id is provably
            // one entry rather than accidentally two.
            let pointers = parse_pointer_sources(&boot);
            if pointers.len() != 2 {
                return Err(format!("{} pointers numbered, want 2: {pointers:?}\n{boot}",
                    pointers.len()));
            }
            if pointers[0].0 != pointers[1].0 {
                return Err(format!(
                    "the two pointers are on slots {} and {}, so a slot-keyed merge would not \
                     have collided and this test proves nothing:\n{boot}",
                    pointers[0].0, pointers[1].0
                ));
            }
            if pointers[0].1 == pointers[1].1 {
                return Err(format!(
                    "both pointers merge as source {} — one of them publishes the other's \
                     buttons:\n{boot}",
                    pointers[0].1
                ));
            }
            serial::Serial::named("boot console", boot.as_str()).must_be_clean()?;
            eprintln!(
                "  [xhci] 2 controllers, 5 HID; both pointers on slot {}, merging as sources {} \
                 and {}",
                pointers[0].0, pointers[0].1, pointers[1].1
            );
            Ok(())
        }
        "xhci_msi_only" => {
            // The T14's Thunderbolt controller printed `xHCI: no MSI-X
            // capability, using polled mode` on a real boot. There was no
            // polled mode: every read of an event ring in this driver is
            // `poll_if_pending`, gated on an `irq_ring` record that only
            // vector 0x21's ISR publishes, and that ISR is delivered only
            // through the MSI-X table the driver had just declined to program.
            // The controller was reset, started, and never read again — with
            // `USB keyboard ready on slot N` printed above it.
            //
            // Every controller in this suite had MSI-X, so this branch had
            // never executed. `msix=off` is the actuator.
            let options = BootOptions {
                profile: qemu::Profile::MetalXhciMsi,
                qmp: true,
                // As in `xhci_second_controller`: with a PS/2 keyboard on the
                // machine, QEMU could deliver the injected keystroke over it
                // and every assertion below would pass with the USB path dead.
                i8042: false,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            // The actuator is a device property, and argv is the only place a
            // device property is visible: a controller that quietly kept its
            // MSI-X table would make this whole test a re-run of the happy
            // path under a different name.
            let controllers = xhci_argv(&argv);
            let [refused, hid] = controllers[..] else {
                return Err(format!("this profile is two controllers; argv has {controllers:?}"));
            };
            if !hid.contains("msix=off") {
                return Err(format!("{hid} still has its MSI-X table"));
            }
            if hid.contains("msi=off") {
                return Err(format!(
                    "{hid} has no MSI either, so there is nothing to fall through to and the \
                     driver is expected to refuse it — that is xhci_no_interrupt"
                ));
            }
            // And the other controller has no interrupt mechanism, so the
            // driver refuses it and never polls it. Without this the test
            // cannot fail: `wait_transfer` drains the entire event ring and
            // dispatches every HID report in it, so a transfer on any polled
            // controller delivers a keyboard's reports with no interrupt
            // anywhere. Measured, not feared — the first shape of this profile
            // passed with MSI deliberately left disabled.
            for want in ["msix=off", "msi=off"] {
                if !refused.contains(want) {
                    return Err(format!(
                        "{refused} still has {want}'s mechanism, so a transfer on it would drain \
                         the HID controller's ring for free"
                    ));
                }
            }
            // Nothing rides USB but the two HID devices; the boot volume is on
            // this profile's NVMe, so no storage transfer can drain a ring.
            if argv.iter().any(|a| a.starts_with("usb-storage")) {
                return Err(format!("a USB disk on a machine that must do no USB storage I/O: {argv:?}"));
            }
            let usb = usb_argv(&argv);
            for want in ["usb-kbd", "usb-mouse"] {
                if !usb.iter().any(|d| d.starts_with(want) && d.contains("bus=xhci1.0")) {
                    return Err(format!("no {want} on the MSI-only controller: {usb:?}"));
                }
            }
            if !argv.iter().any(|a| a.contains("i8042=off")) {
                return Err("the i8042 is on; a PS/2 keyboard could deliver instead".to_string());
            }

            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = serial::Serial::boot(&qemu);

            // What the driver programmed, off its own line. Both halves are
            // needed: MSI-X absent says the actuator did something, MSI
            // present says the driver found the other mechanism rather than
            // refusing the controller.
            boot.must_not_say("xHCI: MSI-X enabled")?;
            boot.must_say("xHCI: MSI enabled (vector 0x21)")?;
            // The line that named a mechanism this driver does not have.
            boot.must_not_say("polled mode")?;
            boot.must_be_clean()?;
            for want in ["keyboard", "mouse"] {
                let binds = parse_xhci_binds(boot.text());
                if binds.iter().filter(|b| b.kind == want).count() != 1 {
                    return Err(format!("{want} not bound exactly once: {binds:?}\n{}",
                        boot.text()));
                }
            }
            // The guest's half of the isolation above: no disk was bound, so
            // nothing in this boot can drain an event ring except an interrupt.
            boot.must_not_say("usb-storage: disk")?;

            // And then the half no log line can show, which is the whole
            // point: a driver that logs `MSI enabled` and programs the
            // capability wrong is indistinguishable from this one until a
            // device actually interrupts. Ground truth is the host's own
            // injection at the device boundary.
            let Some((scale_x, scale_y)) = parse_rel_scale(boot.text()) else {
                return Err(format!("the kernel never said what pointer scale it used:\n{}",
                    boot.text()));
            };
            const DX: i32 = 40;
            const DY: i32 = -30;
            // Off the origin first: the accumulated position clamps at 0, so a
            // move up or left from there is invisible.
            //
            // **`input_events_run`, which is `xhci_second_controller`'s own
            // sequence and was written out again here on fixed sleeps.** Two
            // things came of the copy and both were defects: nothing paced the
            // injection, so a key the host sent while the guest was behind was
            // indistinguishable from one this controller lost — the exact
            // reading `xhci_second_controller` moved off — and nothing
            // sent the right-button release `test_rs_input_events` ends on, so
            // every green run waited out the client's whole 30 s fallback
            // deadline. `input_events_end`'s own doc says every caller owes it
            // one; this was the caller that did not.
            let (result, sent) = input_events_run(&mut qemu, (100, 100), (DX, DY));
            if let Some(err) = &result.error {
                return Err(format!("{err} after {sent} of the sequence\n{}", result.stdout));
            }

            let keys = parse_key_events(&result.stdout);
            let typed: String = keys
                .iter()
                .filter(|e| e.modifiers & 0x10 == 0)
                .map(|e| e.translated.as_str())
                .collect();
            if !typed.contains("hello") {
                return Err(format!(
                    "typed {typed:?}, want it to contain \"hello\" — the keyboard on an \
                     MSI-only controller never reached userland:\n{}",
                    result.stdout
                ));
            }

            let pointer = parse_mouse_events(&result.stdout);
            let want = (DX * scale_x, DY * scale_y);
            let deltas: Vec<(i32, i32)> = pointer
                .windows(2)
                .map(|w| (w[1].x as i32 - w[0].x as i32, w[1].y as i32 - w[0].y as i32))
                .collect();
            if !deltas.contains(&want) {
                return Err(format!(
                    "no pointer event moved by {want:?}; deltas seen: {deltas:?}\n{}",
                    result.stdout
                ));
            }
            let Some(down) = pointer.iter().position(|e| e.buttons == 0x01) else {
                return Err(format!("no left-button-down event; buttons seen: {:?}",
                    pointer.iter().map(|e| e.buttons).collect::<std::collections::BTreeSet<_>>()));
            };
            if !pointer[down + 1..].iter().any(|e| e.buttons == 0x00) {
                return Err(format!("the left button went down and never came up: {pointer:?}"));
            }
            eprintln!(
                "  [xhci] no MSI-X table; MSI took vector 0x21, {} key events (typed {typed:?}), \
                 {} pointer events, delta {want:?} delivered",
                keys.len(),
                pointer.len()
            );
            Ok(())
        }
        "xhci_no_interrupt" => {
            // The terminal case of the same defect: a controller offering
            // neither mechanism. Nothing on a PCIe bus is really built that
            // way, which is exactly why the branch needs staging — "I cannot
            // drive this controller" is a state the driver has to be able to
            // reach and say, and it used to say "using polled mode" instead
            // and then enumerate a keyboard on it.
            //
            // Two controllers, and the crippled one is the second: the first
            // carries the boot stick, so a refusal that took the machine down
            // with it would show up here as a boot that never reaches userland.
            let options = BootOptions {
                profile: qemu::Profile::MetalXhciNoIrq,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            let controllers = xhci_argv(&argv);
            let [good, crippled] = controllers[..] else {
                return Err(format!("this profile is two controllers; argv has {controllers:?}"));
            };
            for want in ["msix=off", "msi=off"] {
                if !crippled.contains(want) {
                    return Err(format!("{crippled} still has {want}'s mechanism"));
                }
            }
            if good.contains("msi") {
                return Err(format!(
                    "{good} is crippled too; then a refusal could not be shown to be per \
                     controller and the machine would have no boot stick"
                ));
            }
            let usb = usb_argv(&argv);
            // The HID is on the controller that will be refused — otherwise
            // "nothing claimed a device" below is true because there was no
            // device to claim, which is not the same statement at all.
            if let Some(bad) = usb
                .iter()
                .filter(|d| !d.starts_with("usb-storage"))
                .find(|d| !d.contains("bus=xhci1.0"))
            {
                return Err(format!("{bad} is not on the controller under test"));
            }
            if !usb.iter().any(|d| d.starts_with("usb-kbd") && d.contains("bus=xhci1.0")) {
                return Err(format!("no keyboard for the driver to refuse: {usb:?}"));
            }
            if !usb.iter().any(|d| d.starts_with("usb-storage") && d.contains("bus=xhci.0")) {
                return Err(format!("the boot stick is not on the good controller: {usb:?}"));
            }

            let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = serial::Serial::boot(&qemu);

            // Both controllers were looked at, one was refused by name, and
            // the refusal says what it means rather than naming a mode.
            if boot.text().matches("xHCI: found at PCI ").count() != 2 {
                return Err(format!("both controllers should be reached:\n{}", boot.text()));
            }
            boot.must_say("xHCI: NOT INITIALISED at PCI")?;
            boot.must_not_say("polled mode")?;

            // And nothing claimed a device on it. This is the assertion the
            // old code failed: it bound the keyboard, printed
            // `USB keyboard ready on slot 2`, and delivered nothing.
            //
            // The needle is certified by the same capture rather than assumed:
            // the boot stick announces itself with it too, so a renamed line
            // reds here instead of making the negative below vacuous.
            const ANNOUNCED: &str = " ready on slot ";
            const BOOT_STICK: &str = "usb-storage: disk 0";
            let carrying: Vec<&str> =
                boot.text().lines().filter(|l| l.contains(ANNOUNCED)).collect();
            if !carrying.iter().any(|l| l.contains(BOOT_STICK)) {
                return Err(format!(
                    "no line carries {ANNOUNCED:?} and {BOOT_STICK:?}, so the absence asserted \
                     below would be the needle's and not the driver's:\n{}",
                    boot.text()
                ));
            }
            let announced: Vec<&&str> =
                carrying.iter().filter(|l| !l.contains(BOOT_STICK)).collect();
            if !announced.is_empty() {
                return Err(format!(
                    "a device was announced on a controller nothing can read: {announced:?}\n{}",
                    boot.text()
                ));
            }
            let binds = parse_xhci_binds(boot.text());
            if !binds.is_empty() {
                return Err(format!(
                    "a device was announced on a controller nothing can read: {binds:?}\n{}",
                    boot.text()
                ));
            }
            boot.must_say("xHCI: 1 controller(s), 0 HID device(s)")?;
            // The good controller is untouched by its neighbour's refusal,
            // and the machine reached userland — `boot_log` ends at the ready
            // marker, so having one at all is that assertion.
            boot.must_say("usb-storage: disk 0 ready")?;
            boot.must_be_clean()?;
            eprintln!(
                "  [xhci] 2 controllers, the second with neither MSI-X nor MSI: refused by \
                 name, 0 HID announced, boot stick on the first still bound"
            );
            Ok(())
        }
        "nvme_large_device" => {
            // Device *size* is a shape dimension, and it is the one nobody had
            // varied: every test image was small enough that an index sized
            // per device block fit under the object allocator's 2 MiB ceiling,
            // so the first boot on the laptop was the first time anything
            // asked for a device-sized allocation — and it died in
            // page_cache::init before it mounted anything.
            let options = BootOptions {
                profile: qemu::Profile::MetalDisk,
                ..Default::default()
            };
            let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let log = qemu.boot_log().to_string();

            // The mechanism, not the argv: a big file on the host proves
            // nothing until the guest's own driver says it enumerated a big
            // namespace. This is the number the T14 printed.
            let Some(blocks) = parse_nvme_blocks(&log) else {
                return Err(format!("the NVMe driver printed no block count:\n{log}"));
            };
            if blocks != qemu::NVME_T14_BLOCKS {
                return Err(format!(
                    "the guest enumerated {blocks} blocks, not the T14's {}",
                    qemu::NVME_T14_BLOCKS
                ));
            }

            // And the cache did not size its index by that number.
            //
            // The bound has to sit *below* the allocator's 2 MiB ceiling to be
            // able to fire at all, which is a narrower window than it looks:
            // a hashbrown index costs 17 B per bucket and its capacities are
            // 7/8 of a power of two, so the last one that fits under the
            // ceiling is 114,688 and the next is unreachable. 16,384 leaves
            // room for a fixed reserve mirroring `slot_to_block`'s 4096 (which
            // rounds up to 7168) and rejects every device-proportional reserve
            // down to one entry per 4 MiB of disk. Measured red at 57,344,
            // which is what `block_count / 1024` asks for and the allocator
            // lets through.
            let Some(index) = parse_page_cache_index(&log) else {
                return Err(format!("the page cache printed no index size:\n{log}"));
            };
            if index > 16_384 {
                return Err(format!(
                    "the block index is sized for {index} blocks on a {blocks}-block device — \
                     that is proportional to the device again:\n{log}"
                ));
            }

            // The whole storage stack on the real geometry, not just the boot:
            // format, allocate, write, read back.
            let result = qemu.run_test("test_rs_nvme_home_roundtrip", Duration::from_secs(20));
            if !check_rust_result(&result) {
                return Err(format!(
                    "the /home round trip failed on a {blocks}-block device:\n{}",
                    result.stdout
                ));
            }

            // Then shut down, which is the only thing that runs the page
            // cache's write-back over every dirty slot the format left —
            // ~1900 of them on a device this size against 8 on the small one,
            // so the coalescing loop is only ever exercised at scale here.
            //
            // The kernel's own shutdown lines are observable now: the ring
            // is drained in `acpi::shutdown()` before it cuts the power.
            // Asserted below, because "how far did the sync get" is the only
            // diagnostic a shutdown failure has, and on a machine with no
            // serial it is the only channel there is.
            let image = qemu.nvme_image().to_path_buf();
            writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
            qemu.flush_stdin();
            let tail = qemu.drain_serial(Duration::from_secs(20));

            for line in ["Syncing filesystems...", "Shutting down."] {
                if !tail.contains(line) {
                    return Err(format!(
                        "{line:?} never reached the host — the ring was still \
                         holding it when the power was cut:\n{tail}"
                    ));
                }
            }

            // The shutdown half is the one this conversion is for: `tail` is a
            // `drain_serial` window, and an empty drain used to pass its panic
            // scan in silence. It carries kernel lines of its own -- measured,
            // five, including both lines asserted just above -- so requiring
            // liveness of it is a real check and not a new flake.
            serial::Serial::named("boot console", log.as_str()).must_be_clean()?;
            serial::Serial::named("shutdown drain", tail.as_str()).must_be_clean()?;

            // Ground truth at the hardware boundary: the backing file is what
            // the *device* received, so this is the one place a storage claim
            // does not rest on the guest's account of itself. The clean flag
            // reaches the platter only through `PageCache::sync`, and the
            // backup superblock only through a write at the far end of DATA on
            // a 244 GB device. Where DATA is comes out of the table, never out
            // of an offset this side computed.
            let (data_at, data_bytes) = toyos_build::image::data_partition_of(&image)?;
            let (first, data_blocks) = (data_at / 4096, data_bytes / 4096);
            for (name, block) in [("primary", first), ("backup", first + data_blocks - 1)] {
                let sb = read_superblock(&image, block)
                    .map_err(|e| format!("{name} superblock at block {block}: {e}"))?;
                if sb.block_count != data_blocks {
                    return Err(format!(
                        "the {name} superblock was formatted for {} blocks, not the \
                         {data_blocks} of the DATA partition on a {}-block device",
                        sb.block_count,
                        qemu::NVME_T14_BLOCKS
                    ));
                }
                if !sb.is_clean() {
                    return Err(format!(
                        "the {name} superblock is not marked clean — the write-back at \
                         shutdown did not reach the device"
                    ));
                }
            }

            // And the image is still sparse. A materialized one is how a test
            // disk ends up small enough to hide this class of bug in the first
            // place, and 244 GB of zeros is not something to leave on a laptop.
            let (apparent, allocated) = image_extent(&image);
            if apparent != qemu::NVME_T14_BYTES {
                return Err(format!("the image is {apparent} bytes, want {}", qemu::NVME_T14_BYTES));
            }
            if allocated > 1024 * 1024 * 1024 {
                return Err(format!(
                    "the image occupies {allocated} bytes of the host's disk — it is not sparse"
                ));
            }
            eprintln!(
                "  [nvme] {blocks} blocks, index sized for {index}; both superblocks clean; \
                 image {} MiB on disk of {} GB apparent",
                allocated / (1024 * 1024),
                apparent / 1_000_000_000
            );
            Ok(())
        }
        "nvme_wide_sector" => {
            // The other half of "a device's size is a shape dimension": not how
            // many sectors, but how big one is. `lba_ds` is an 8-bit
            // device-reported shift that reached `1 << lba_ds` and then
            // `4096 / sector_size`, so an 8 KiB-format namespace divided by
            // zero at 0.068 s — before storage, before a console, and on a
            // machine whose only channel out is the one that does not exist
            // yet. Every profile in this tree took QEMU's implicit 512-byte
            // namespace, so nothing could ask.
            //
            // The guest is expected to die here, which is what makes
            // `ready_marker` the driver's own refusal: anything but
            // DEFAULT_READY tells the harness a panic is the outcome under
            // test rather than a boot failure.
            const REFUSAL: &str = "NVMe: namespace reports";
            let options = BootOptions {
                profile: qemu::Profile::NvmeWideSector,
                ready_marker: REFUSAL,
                ..Default::default()
            };
            let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            // This profile has no virtio-serial, so stdio *is* the 16550 and
            // `boot_log` is the whole record. It ends at the refusal, which is
            // the driver's `assert!`: nothing downstream of it can run.
            let log = serial::Serial::boot(&qemu);

            // Named, not just refused: the value the device reported is the
            // whole diagnostic on a machine that will not boot again without
            // it. A bare "refused" line would pass with the number wrong.
            log.must_say("2^13-byte sectors")?;
            // And it refused rather than dividing: the pre-fix failure was
            // `attempt to divide by zero`, which is also a panic and would
            // satisfy the check above if it only looked for one. Both of these
            // are absence claims, so both go through `must_not_say`, which
            // fails rather than passing if the capture came back empty.
            log.must_not_say("divide by zero")?;
            // Nothing downstream ran. `block device id=` is the line
            // `NvmeBlockDevice::new` logs, and it is the call that divided.
            log.must_not_say("NVMe: block device id=")?;
            eprintln!("  [nvme] 8 KiB-format namespace refused by name, before storage came up");
            Ok(())
        }
        "va_exhaustion" => {
            // `find_gap` returning None was an `.expect` on five paths. It is
            // an error return now, and this is the only way to reach it: the
            // arena is ~1015 GB and every region in it costs at worst twice
            // its size in physical memory, so the PMM refuses hundreds of
            // gigabytes before the address space does. `test-tiny-va` moves
            // the floor and nothing else — the argument for the actuator is on
            // `vma::ALLOC_FLOOR`.
            //
            // Which is also why the feature has to boot a whole system: an
            // arena too small for a process to map its TLS and its heap would
            // prove the actuator works and nothing about the kernel.
            let options = BootOptions {
                kernel_params: &["test-tiny-va"],
                ..Default::default()
            };
            let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();

            let result = qemu.run_test("test_rs_va_exhaustion", Duration::from_secs(30));
            if !check_rust_result(&result) {
                return Err(format!("the guest did not survive exhaustion:\n{}", result.stdout));
            }
            // The guest asserts the mapping count itself — the band that
            // separates "address space ran out" from "memory ran out". Here,
            // that nothing in the kernel panicked on the way: the process
            // exiting 0 says its own syscalls returned, not that some other
            // CPU stayed up.
            //
            // Two captures, two `Serial`s rather than one with the second
            // pushed into it: concatenating them would let the boot half's
            // kernel lines vouch for the run half's liveness, which is the
            // vacuum this is being converted out of. Measured: the run window
            // carries 14 kernel lines of its own.
            serial::Serial::named("boot console", boot).must_be_clean()?;
            serial::Serial::named("test serial", result.serial.as_str()).must_be_clean()?;
            eprintln!("  [va] {}", result.stdout.trim());
            Ok(())
        }
        "readdir_bound" => {
            // Two defects, one workload, no kernel feature: `Vfs::list` had no
            // cap and `SYS_READDIR` reported the bytes it managed to write, so
            // a directory of 32,769 files panicked the kernel and one of 34,816
            // came back as 4125 entries and a success.
            //
            // Its own boot because it fills `/tmp` to the listing limit and
            // leaves it there — in the shared boot every later
            // `read_dir("/tmp")` would be refused, which is a cascade rather
            // than a failure.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions::default(),
            );
            serial::Serial::boot(&qemu).must_be_clean()?;

            // 120 s: the `/home` arm alone is 32,769 creates on bcachefs.
            let result = qemu.run_test("test_rs_readdir_bound", Duration::from_secs(120));
            if let Some(err) = &result.error {
                return Err(format!("the guest stopped answering: {err}\nserial:\n{}", result.serial));
            }
            if !check_rust_result(&result) {
                return Err(format!("readdir_bound failed:\n{}", result.stdout));
            }
            // The refusal must be an error return and nothing else. A panic
            // inside `Vfs::list` is the defect this replaced, and the guest
            // process exiting 0 does not rule one out on another CPU.
            serial::Serial::named("test serial", result.serial.as_str()).must_be_clean()?;
            for line in result.stdout.lines().filter(|l| l.contains("PASS")) {
                eprintln!("  [readdir]{}", line.trim_start_matches("  PASS"));
            }
            Ok(())
        }
        "mkdir_cap" => {
            // `Vfs::create_dir` grew a kernel `HashSet` without a ceiling, so a
            // `mkdir` loop ran the heap out. Its own boot: it fills the cap and
            // leaves it there, which a shared boot's later `mkdir`s would trip.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions::default(),
            );
            serial::Serial::boot(&qemu).must_be_clean()?;

            let result = qemu.run_test("test_rs_mkdir_cap", Duration::from_secs(60));
            if let Some(err) = &result.error {
                return Err(format!("the guest stopped answering: {err}\nserial:\n{}", result.serial));
            }
            if !check_rust_result(&result) {
                return Err(format!("mkdir_cap failed:\n{}", result.stdout));
            }
            // The refusal is an error return and nothing else: a panic in the VFS
            // would strand its lock, which the guest exiting 0 does not rule out.
            serial::Serial::named("test serial", result.serial.as_str()).must_be_clean()?;
            for line in result.stdout.lines().filter(|l| l.contains("PASS")) {
                eprintln!("  [mkdir]{}", line.trim_start_matches("  PASS"));
            }
            Ok(())
        }
        "fpu_isolation" => {
            // Two boots that must answer
            // differently: the shipped kernel preserves the whole user machine
            // state across every transition out of Ring 3, and the kernel built
            // with `fpu-save-nothing` — the same bracket with the two FP
            // instructions taken out — must fail the same three arms.
            //
            // Without the second arm the first proves only that the machine
            // works, which it did before this gate existed too.
            //
            // smp=1 in both, and that is the stronger machine rather than the
            // weaker one: two of the arms are about a register file surviving
            // from one process to the next, which needs the two to share a CPU.
            // On the shared boot's two CPUs that is a coin flip, which is why
            // CI's own observation of the defect was intermittent.
            let one_cpu = || BootOptions { smp: 1, ..BootOptions::default() };

            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, one_cpu());
            serial::Serial::boot(&qemu).must_be_clean()?;
            let result = qemu.run_test("test_rs_fpu_isolation", Duration::from_secs(120));
            if let Some(err) = &result.error {
                return Err(format!(
                    "the guest stopped answering: {err}\nserial:\n{}",
                    result.serial
                ));
            }
            if !check_rust_result(&result) {
                return Err(format!("fpu_isolation failed:\n{}", result.stdout));
            }
            // No `must_be_clean` on the run: arm 2 kills `fault_gate_child`
            // on purpose, and the kernel names every Ring 3 fault it takes.
            for line in result.stdout.lines() {
                eprintln!("  [fpu] {}", line.trim());
            }
            // The lane's disk images are one set, and QEMU takes a write lock
            // on them for as long as a guest lives.
            drop(qemu);

            let mut blind = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions { kernel_features: &["fpu-save-nothing"], ..one_cpu() },
            );
            serial::Serial::boot(&blind).must_be_clean()?;
            let negative = blind.run_test("test_rs_fpu_isolation", Duration::from_secs(120));
            if let Some(err) = &negative.error {
                return Err(format!(
                    "the negative-control guest stopped answering: {err}\nserial:\n{}",
                    negative.serial
                ));
            }
            if negative.exit_code == Some(0) {
                return Err(format!(
                    "the kernel built with `fpu-save-nothing` passed `fpu_isolation`, so the \
                     gate asserts nothing:\n{}",
                    negative.stdout
                ));
            }
            eprintln!(
                "  [fpu] fpu-save-nothing: exit {:?}, which is the gate having teeth",
                negative.exit_code
            );
            Ok(())
        }
        "gsbase_locked" => {
            // Two boots that must differ: the shipped kernel #UDs the GS-base
            // primitive; `user-writable-gsbase` (the fix reverted) leaves it.
            let one_cpu = || BootOptions { smp: 1, ..BootOptions::default() };

            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, one_cpu());
            serial::Serial::boot(&qemu).must_be_clean()?;
            let result = qemu.run_test("test_rs_gsbase_locked", Duration::from_secs(120));
            if let Some(err) = &result.error {
                return Err(format!("the guest stopped answering: {err}\nserial:\n{}", result.serial));
            }
            if !check_rust_result(&result) {
                return Err(format!("gsbase_locked failed on the shipped kernel:\n{}", result.stdout));
            }
            // Reached and refused, not skipped: the #UD is the kernel's `SIGILL`.
            if !result.serial.contains("SIGILL") {
                return Err(format!(
                    "gsbase_locked passed but no SIGILL: the probe never reached the instruction\n{}",
                    result.serial
                ));
            }
            for line in result.stdout.lines() {
                eprintln!("  [gsbase] {}", line.trim());
            }
            drop(qemu);

            let mut blind = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions { kernel_features: &["user-writable-gsbase"], ..one_cpu() },
            );
            serial::Serial::boot(&blind).must_be_clean()?;
            let negative = blind.run_test("test_rs_gsbase_locked", Duration::from_secs(120));
            if let Some(err) = &negative.error {
                return Err(format!(
                    "the negative-control guest stopped answering: {err}\nserial:\n{}",
                    negative.serial
                ));
            }
            if negative.exit_code == Some(0) {
                return Err(format!(
                    "the `user-writable-gsbase` kernel passed `gsbase_locked`, so the gate \
                     asserts nothing:\n{}",
                    negative.stdout
                ));
            }
            // Present, not merely a different exit: a kernel-half GS base leaked.
            let leaked = negative
                .stdout
                .lines()
                .chain(negative.serial.lines())
                .filter_map(|l| l.split("base=0x").nth(1))
                .filter_map(|h| u64::from_str_radix(h.split_whitespace().next().unwrap_or(""), 16).ok())
                .find(|&b| b >= 0xffff_8000_0000_0000);
            if leaked.is_none() {
                return Err(format!(
                    "the control exited nonzero but leaked no kernel GS base:\n{}",
                    negative.stdout
                ));
            }
            eprintln!(
                "  [gsbase] user-writable-gsbase: exit {:?}, which is the gate having teeth",
                negative.exit_code
            );
            Ok(())
        }
        "sched_check_build" => {
            // The scheduler core's own instruments, run on a real machine —
            // the on-target counterpart to everything the simulator
            // does.
            //
            // `kernel/Cargo.toml` has forwarded `sched-check =
            // ["toyos-sched/check"]` since the check build was written, and
            // until this test nothing in `src/` or `tests/` ever asked for it.
            // So `cpu::MAX_PASS_NS`, the pass-cost measurement and
            // `invariants::check_cpu` were compiled by no CI run at all: a
            // quantum never armed, a task whose container disagreed with its
            // state word, and a distribution of passes with mass over the
            // budget were each caught by nothing on hardware, however green the
            // simulator was.
            //
            // **The workload is `sched_stress`** because the asserts are dense
            // on exactly what it does: it spawns burners that drive vruntime,
            // blocks and wakes across io_uring and ports, and forces a
            // Runnable→NonRunnable→Runnable cycle. Every one of those is a pass,
            // and every pass on every CPU runs both checks. `smp: 2` is the
            // default and it is deliberate — one CPU cannot migrate, and
            // invariant T's arming is per CPU.
            //
            // **That the asserts are compiled in at all is not asked here.** A
            // guest proves they did not fire, and a kernel with the feature
            // quietly dropped proves that more easily; the artifact is asked
            // instead, at build time, by `assert_sched_check_matches_features` —
            // 0 of 3 assert texts in the shipping kernel, 3 of 3 in this one.
            // This half is the other one: on a machine that really carries them,
            // honest work does not trip them.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_features: toyos_build::build::SCHED_CHECK_KERNEL,
                    ..BootOptions::default()
                },
            );
            // The boot is already thousands of passes on both CPUs, and an
            // assert that fires there takes the machine down before userland.
            serial::Serial::boot(&qemu).must_be_clean()?;

            let result = qemu.run_test("test_rs_sched_stress", Duration::from_secs(120));
            if let Some(err) = &result.error {
                return Err(format!(
                    "the check-build guest stopped answering, which is what a scheduler \
                     assert firing looks like from here: {err}\nserial:\n{}",
                    result.serial,
                ));
            }
            if !check_rust_result(&result) {
                return Err(format!(
                    "sched_stress failed on the check build:\n{}",
                    result.stdout
                ));
            }
            // An assert fires as a kernel panic on whichever CPU took the pass,
            // and the guest process can still exit 0 while another CPU is dying
            // — so the serial is read as well as the exit code.
            serial::Serial::named("test serial", result.serial.as_str()).must_be_clean()?;
            for line in result.stdout.lines() {
                eprintln!("  [sched-check] {}", line.trim());
            }
            // The instrument published, read back through the parser its format
            // is held to: a report from every CPU, over the whole boot in the
            // three pieces a capture comes in. What a report says is not judged
            // here — a pass's cost is a duration.
            let mut capture = serial::Serial::boot(&qemu);
            capture.push(&result.before);
            capture.push(&result.serial);
            let reported: BTreeSet<u32> = capture
                .text()
                .lines()
                .filter_map(toyos_sched::cpu::PassCostReport::parse)
                .map(|report| report.cpu.0)
                .collect();
            let cpus = BootOptions::default().smp;
            if reported.len() != cpus as usize {
                return Err(format!(
                    "the check build published pass-cost reports from cpus {reported:?} of the \
                     {cpus} it boots: `{}` is the prefix the rest never printed\n{}",
                    toyos_sched::cpu::PassCostReport::PREFIX,
                    capture.text(),
                ));
            }
            Ok(())
        }
        "klogd_hosted" => {
            // The machine's first kernel thread, and the thing about it no
            // other test in the suite can see: **that it is hosted at all.**
            // `klogd` runs on the ordinary scheduler with no address space of
            // its own — `driver::spawn` names the kernel's `cr3` — through a
            // trampoline that never issues an `iretq`. It gets a process-table
            // entry rather than a bare task, and that is what makes it
            // nameable: without one a crash report would print a pid nothing
            // in the machine resolves.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions::default(),
            );
            klogd_hosted(&serial::Serial::boot(&qemu))
        }
        "klogd_fault_halts" => power::klogd_death_resets(
            test_config,
            c_bins,
            rust_bins,
            &["klogd-fault", "panic-reboot-fast"],
            &["KERNEL PANIC: read unmapped address at 0x0", "console::body"],
        ),
        "syscall_panic_halts" | "syscall_fault_halts" | "lock_across_switch_halts"
        | "heap_over_ceiling_halts" => {
            use toyos_abi::syscall::{debug_action as da, SYS_DEBUG};
            let syscall = format!("Syscall: num={SYS_DEBUG}");
            let syscall = syscall.as_str();
            let (action, said): (u64, &[&str]) = match name {
                "syscall_panic_halts" => (
                    da::PANIC,
                    &["SYS_DEBUG: kernel panic triggered by userspace", syscall, "User backtrace:"],
                ),
                // A Ring 0 read of a user address is the kernel's, inside a syscall too.
                "syscall_fault_halts" => (da::NULL_READ, &[syscall, "User backtrace:"]),
                "lock_across_switch_halts" => (da::LOCK_ACROSS_SWITCH, &[syscall]),
                // The message, not `mm/alloc.rs`: it names the ceiling rather
                // than the page source's own request.
                "heap_over_ceiling_halts" => {
                    (da::HEAP_OVER_CEILING, &["exceeds MAX_HEAP_ALLOC", syscall])
                }
                other => unreachable!("{other} is not a syscall-death row"),
            };
            let said = power::syscall_death_resets(test_config, c_bins, rust_bins, action, said)?;
            // With the capture: this guest's 16550 is its stdio, so no
            // `uart-*.log` keeps what it said.
            match name {
                "lock_across_switch_halts" => check_tripwire_attribution(&said),
                "syscall_fault_halts" => check_ring0_read_unmapped(&said),
                _ => Ok(()),
            }
            .map_err(|e| format!("{e}\n{said}"))
        }
        "hash_seed_precedes_every_map" => {
            // `kernel/src/hasher.rs`'s `UNSEEDED`, as a prefix: the wrong seed
            // the compiler cannot reach, because the container works. Its other
            // two are unrepresented here — both x86-64 CPU models carry `+rdrand`
            // (`Arch::cpu`) and QEMU's DRNG always answers — so
            // the no-source refusal and `NO_ENTROPY` are mutation-measured.
            const UNSEEDED: &str = "kernel hasher: a hash container was built before hasher::seed()";
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["test-hash-before-seed"],
                    ready_marker: UNSEEDED,
                    ..Default::default()
                },
            );
            let mut said = serial::Serial::boot(&qemu);
            said.push(&qemu.uart_log());
            let line = said.must_say(UNSEEDED)?;
            eprintln!("  [hasher] {}", line.trim());
            // Before `mm::init`, so the boot does not finish.
            said.must_not_say(qemu::DEFAULT_READY)?;
            Ok(())
        }
        "reentry_names_the_first_panic" => {
            // **The one class of crash that is by definition two bugs deep, and
            // the one class that used to leave no evidence.** A machine two
            // crashes deep said `DOUBLE PANIC` and nothing else — not what the
            // first crash was, not where, and not what the second one was
            // (`issues/panic-path/a-double-panic-at-boots-edge-says-nothing-but-its-name.md`).
            // What closed it is a bounded byte copy taken *before* either
            // report runs, into a static reserved at link time
            // (`kernel/src/panic.rs`), so what the second crash reads is the
            // first crash's own words rather than whatever the log path
            // survived.
            //
            // **Two names because the two dead ends are reached by different
            // accidents**, and `double_panic_names_the_fault` is the other. The
            // reentry guard fires when the panic *report* panics, on a CPU whose
            // panic depth is already one; `DOUBLE PANIC` fires when a panic
            // lands on a CPU that a *fault* had, whose depth is zero.
            //
            // **This one: the panic path panics.** `test-late-panic` is a real
            // panic with a literal message at a fixed site, and
            // `panic-in-report` kills the report of it before it says a word —
            // so everything on the wire about the first panic came out of the
            // capture. The reentry guard writes straight to the 16550 with no
            // lock, deliberately, because the record path is exactly what has
            // just failed: the marker and the verdict are both in the UART file
            // rather than on the console.
            const REENTRY: &str = "PANIC REENTRY";
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["test-late-panic", "panic-in-report"],
                    ready_marker: REENTRY,
                    ..Default::default()
                },
            );
            let mut reentry = serial::Serial::boot(&qemu);
            reentry.push(&qemu.uart_log());
            // Nothing of the first panic reached the record ring: the report
            // that writes `PANIC:` is the one that died. So this is not a
            // weaker way of reading the same line — without the capture there
            // is no other copy of the site anywhere in the capture.
            reentry.must_not_say("PANIC:")?;
            let header = reentry.must_say(REENTRY)?;
            eprintln!("  [reentry] {}", header.trim());
            let first = reentry.must_say("first (apic")?;
            // `src/main.rs` and not `kernel/src/main.rs`: `build.rs` runs cargo
            // in `kernel/`, so the kernel's own `file!()` is crate-relative.
            for want in ["panic at ", "src/main.rs:", "test-late-panic: on-screen console check"] {
                if !first.contains(want) {
                    return Err(format!(
                        "the reentry report does not carry {want:?} — the first panic's own \
                         words are what the capture exists to keep: {first:?}"
                    ));
                }
            }
            eprintln!("  [reentry] {}", first.trim());
            let second = reentry.must_say("second: panic at")?;
            if !second.contains("panic-in-report: the crash report panicked") {
                return Err(format!(
                    "the reentry report does not name the second panic: {second:?}"
                ));
            }
            eprintln!("  [reentry] {}", second.trim());
            Ok(())
        }
        "double_panic_names_the_fault" => {
            // **A panic on top of a fault, which is what the sighting was and
            // what no test in this tree had ever executed**: a Ring 0 exception
            // is not something a guest program or a QEMU property can produce,
            // so `fatal_exception`'s kernel arm — and the `DOUBLE PANIC` branch
            // only reachable through it — had never run under a test at all.
            // `reentry_names_the_first_panic` is the other dead end and carries
            // the shared argument.
            //
            // `test-kernel-fault` takes the `#UD` with nothing current, so
            // `fatal_exception` runs its kernel arm; `panic-in-report` panics it
            // before the `FAULT rip=…` line, which is the shape the sighting had
            // — a fault whose report died before saying anything at all. This
            // dead end says it as a record too, because a machine with no serial
            // port has no other channel, so the verdict is on the console.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["test-kernel-fault", "panic-in-report"],
                    ready_marker: "DOUBLE PANIC",
                    ..Default::default()
                },
            );
            let mut double = serial::Serial::boot(&qemu);
            double.push(&qemu.uart_log());
            // The fault said nothing about itself, which is the state under
            // test: `FAULT rip=…` is `fatal_exception`'s own first line and it
            // never ran.
            double.must_not_say("FAULT rip=")?;
            let line = double.must_say("DOUBLE PANIC")?;
            for want in [
                // Which of the four states the arriving panic found. A panic on
                // top of a fault and a panic on top of a panic are different
                // machines and the old line named neither.
                "already in Fatal",
                // The fault, by the same name the report it never reached would
                // have given it, and where it was.
                "invalid opcode",
                "rip=0x",
                // And the panic that ended it.
                "second: panic at ",
                "panic-in-report: the crash report panicked",
            ] {
                if !line.contains(want) {
                    return Err(format!(
                        "the DOUBLE PANIC line does not carry {want:?}, so the machine still \
                         dies without saying what it was already doing: {line:?}"
                    ));
                }
            }
            eprintln!("  [double] {}", line.trim());
            // And the same report on the channel that cannot be held by
            // whatever broke — the raw port write goes out before the record
            // does, so a wedge in the log path costs the second copy and never
            // the first.
            let raw = serial::Serial::named("16550 file", qemu.uart_log());
            let raw_line = raw.must_say("first (apic")?;
            if !raw_line.contains("invalid opcode") {
                return Err(format!(
                    "the lock-free copy of the report does not name the fault: {raw_line:?}"
                ));
            }
            eprintln!("  [double] {}", raw_line.trim());
            Ok(())
        }
        "nested_fault_is_recursive" => {
            // **The third second-failure shape, and the one that was silently
            // misclassified.** `reentry_names_the_first_panic` stages a panic
            // inside a panic and `double_panic_names_the_fault` a panic on top
            // of a fault; this stages a `#PF` inside a panic, which is the case
            // `fatal_exception`'s recursive short-circuit was written for.
            //
            // `page_fault_handler` swaps this CPU's fault state to `PageFault`
            // before it looks at what was there, so until the fix the nested
            // `#PF` arrived at `fatal_exception` looking like the first crash on
            // the CPU: the branch printed no `RECURSIVE` and ran the whole
            // second report. `test-late-panic` is the first crash and
            // `fault-in-report` is the wild read inside its report.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["test-late-panic", "fault-in-report", "panic-reboot-fast"],
                    ready_marker: "RECURSIVE",
                    ..Default::default()
                },
            );
            let mut nested = serial::Serial::boot(&qemu);
            nested.push(&qemu.uart_log());
            let line = nested.must_say("RECURSIVE")?;
            if !line.contains("FAULT rip=") {
                return Err(format!(
                    "`RECURSIVE` is not on `fatal_exception`'s own line, so it is some other \
                     word: {line:?}"
                ));
            }
            eprintln!("  [nested] {}", line.trim());
            // And the branch bounds what it claims to: the arm that fires skips
            // `crash_report`, so the nested fault writes no second report. The
            // first panic's report never ran either — the wild read is at its
            // head. `KERNEL PANIC:` is `crash_report_exception`'s own header;
            // the stack scan is `double_fault_handler`'s, which is the
            // escalation and not the report. Judged past the marker, over the
            // capture the fatal path's reset closes: `boot_log` stops at
            // `RECURSIVE` and the report would follow it there.
            const REPORTS: [&str; 2] = ["KERNEL PANIC:", "Scanning kernel stack at"];
            let mut tail = String::new();
            qemu::await_reset(&mut qemu, &mut tail, "the fatal path to reset the machine", &REPORTS)?;
            nested.push(&tail);
            for report in REPORTS {
                nested.must_not_say(report)?;
            }
            eprintln!("  [nested] the recursive arm bounded the report: no second crash report");
            Ok(())
        }
        "pre_idle_wedge_speaks" => {
            // **The worst diagnostic hole in the tree, closed and gated.**
            // Before this branch a boot that wedged before `enter_idle_loop`
            // produced nothing at all on the console — not "less", *nothing*,
            // including every line it had logged — because the only two things
            // that drained the byte ring were the timer tick and the idle loop,
            // and the machine reaches neither. It cost an hour the first time
            // it was met: a mis-programmed IOMMU stopped NVMe mid-`init`, the
            // guest had logged sixty lines, and the harness saw the
            // bootloader's output and then a ten-second timeout.
            // `Drain::Inline` puts every record on the wire as it is committed,
            // for the whole boot, so the end of the log is now where the
            // machine stopped rather than where it last drained.
            //
            // The verdict is which lines arrived, from the first phase to the
            // wedge; nothing follows it because the wedge never returns.
            const WEDGE: &str = "pre-idle-wedge: the boot stops here";
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    // **`Metal`, because the console has to exist in phase 1
                    // for the claim to mean anything.** The headless profile's
                    // console is a virtio device the kernel does not bring up
                    // until phase 6, so a machine wedged in phase 3 has nowhere
                    // to put a byte on that shape and the records wait in their
                    // shards for a backend that never arrives. metal-sim keeps
                    // a 16550, which is up from the second statement of
                    // `kernel_main` — and it is also the profile this whole
                    // feature exists for, being the shape that gets flashed.
                    profile: qemu::Profile::Metal,
                    kernel_params: &["pre-idle-wedge"],
                    ready_marker: WEDGE,
                    ..Default::default()
                },
            );
            let boot = serial::Serial::boot(&qemu);
            // Every phase up to the wedge, oldest first — the first line the
            // machine ever logs, a line from between the first two checkpoints,
            // and the storage phase the wedge follows.
            for needle in [
                "serial: 16550 loopback read",
                "Boot: CPU ready",
                "gpt: firmware booted us from partition",
                "Boot: peripherals ready",
                "Boot: storage ready",
                WEDGE,
            ] {
                boot.must_say(needle)?;
            }
            eprintln!(
                "  [wedge] {} kernel line(s) reached the console from a machine that never \
                 reached a scheduler pass",
                boot.kernel_lines(),
            );
            Ok(())
        }
        "short_sleep_livelock" => {
            // Task #156. A `nanosleep` whose deadline is already past when the
            // pass arms the one-shot armed the register's one-tick minimum, and
            // the Ring 0 timer stub reloads whatever was last armed — so the
            // CPU took that interrupt again before it could execute the
            // instruction after the `wrmsr` that armed it, forever. Eight boots
            // of the owner's T14 caught it twice by NMI, at
            // `arm_one_shot+0x8d` and at `timer_entry+0x0`, which are the two
            // instruction boundaries of exactly that loop.
            //
            // Its own boot because the failure is a CPU that never runs
            // anything again: on the shared boot it would be reported against
            // whichever test followed it.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions::default(),
            );
            serial::Serial::boot(&qemu).must_be_clean()?;

            let result = qemu.run_test("test_rs_abuse_short_sleep", Duration::from_secs(60));
            if let Some(err) = &result.error {
                return Err(format!(
                    "a sleep shorter than one LAPIC tick took the CPU with it: {err}\nserial:\n{}",
                    result.serial,
                ));
            }
            if !check_rust_result(&result) {
                return Err(format!("abuse_short_sleep failed:\n{}", result.stdout));
            }
            serial::Serial::named("test serial", result.serial.as_str()).must_be_clean()?;
            eprintln!("  [sleep] {}", result.stdout.lines().last().unwrap_or("").trim());
            Ok(())
        }
        "heap_ceiling_bounds" => {
            // Its own boot: `LOWER_SYSINFO_BOUND` stays lowered for the rest of it.
            let options = BootOptions { kernel_features: ACTUATOR_KERNEL, ..Default::default() };
            let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            serial::Serial::boot(&qemu).must_be_clean()?;

            let result = qemu.run_test("test_rs_heap_ceiling", Duration::from_secs(30));
            if let Some(err) = &result.error {
                return Err(format!("heap_ceiling did not finish: {err}\nserial:\n{}", result.serial));
            }
            if !check_rust_result(&result) {
                return Err(format!("heap_ceiling failed:\n{}", result.stdout));
            }
            Ok(())
        }
        "cache_eviction" => {
            // Both disk caches grew for the life of the boot: nothing ever
            // removed a block-cache slot, and the file cache's budget was
            // `usize::MAX` because the one function that would have set it had
            // no callers. This drives the bounds that replaced that.
            //
            // `test-small-caches` is the actuator for the same reason
            // `xhci-one-slot` is: the shipped bounds are 16 MiB and 64 MiB on
            // this guest, and filling them by doing real I/O is minutes of
            // NVMe traffic to observe a policy that 256 KiB observes in a
            // second. The eviction code is the shipped code — only the number
            // moves, and the boot line below is what proves which number is in
            // force.
            // The T14's namespace, because the two caches are filled by
            // different things. File pages come from the guest program below;
            // metadata blocks come from the *device*, whose allocator bitmap
            // is one bit per block — 1900 blocks of it on a 244 GB namespace
            // against 8 on the 128 MiB one, which is the difference between
            // overflowing a 64-slot cache during the format and never
            // reaching it. Measured: 0 block-cache evictions on Headless.
            //
            // And it has to be an *unformatted* namespace, which the harness
            // gives every boot that names no image: a mount of one an earlier
            // boot formatted reads a handful of metadata blocks and evicts
            // nothing, and the turnover assertion below goes red on that
            // rather than vacuously green.

            // `nvme-spent-budget` and `nvme-command-silent` ride this boot
            // rather than buying registered names of their own: each needs the
            // test kernel and a real NVMe namespace, which is what this test
            // already boots. The first costs one refused read before anything
            // mounts the device — no command issued, no cache slot taken; the
            // second costs one abandoned read and the controller reset that
            // reclaims it, all before the mount, so the eviction series below
            // runs on the freshly rebuilt queues — which is itself half the
            // point: a reset that left them out of step reds the series.
            let options = BootOptions {
                profile: qemu::Profile::MetalDisk,
                kernel_params: &["test-small-caches", "nvme-spent-budget", "nvme-command-silent"],
                ..Default::default()
            };
            let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();

            // The NVMe half of `block::OPERATION`. `usb-storage-gate` asserts
            // the same refusal on the USB path; this is the one taken with both
            // page-cache locks held, which is what made a missing deadline a
            // wedged CPU rather than a slow read.
            //
            // Both lines, and the second is the one easy to leave out: a driver
            // that refused by abandoning a command in flight would pass the
            // first and fail here, because the queue would still be owed a
            // completion and the DMA window still owed a write.
            let console = serial::Serial::named("boot console", boot.as_str());
            console.must_say("nvme-gate: read with a spent budget refused=true budget=true")?;
            console.must_say("nvme-gate: the same block read afterwards ok=true")?;

            // The reset escalation, NVMe 2.0 §3.7.2: a command whose completion
            // wait was skipped is a live controller owing an answer, and until
            // 2026-08-23 that single silence was a disk declared dead. Now it
            // must be one reset, the silence answered as a budget word rather
            // than a device fact, and the same block readable through the
            // rebuilt queues.
            console.must_say("nvme-gate: the silent command's read refused=true budget=true")?;
            console.must_say("NVMe: controller reset complete")?;
            console.must_say("nvme-gate: the same block read after the reset ok=true")?;

            let Some(file_budget) = parse_cache_budget(&boot, "file cache: budget ") else {
                return Err(format!("the file cache printed no budget:\n{boot}"));
            };
            let Some(block_budget) = parse_cache_budget(&boot, "cached blocks, cap ") else {
                return Err(format!("the block cache printed no slot cap:\n{boot}"));
            };
            if file_budget != 64 || block_budget != 64 {
                return Err(format!(
                    "budgets are {file_budget} file pages and {block_budget} block slots, \
                     not the 64 each the feature asks for — the bound under test is not the \
                     one the workload was sized against:\n{boot}"
                ));
            }

            let result = qemu.run_test("test_rs_cache_eviction", Duration::from_secs(180));
            if !check_rust_result(&result) {
                return Err(format!(
                    "a page did not survive being evicted and re-read:\n{}\n{}",
                    result.stdout, result.serial
                ));
            }

            // The whole point, and the half a compile cannot fake: residency
            // is flat while the eviction count climbs. Boot and test output
            // both, since the block cache starts evicting during the format.
            let log = format!("{boot}\n{}", result.serial);
            let file_series = parse_file_cache_series(&log);
            let block_series = parse_cache_series(&log, "page cache: ", "slots resident");

            // One turnover line means one eviction happened and nothing
            // more; the workload is 8x the budget in each cache, so a
            // series this short means eviction is not keeping up with the
            // pressure — or is not running at all.
            if file_series.len() < 4 {
                return Err(format!(
                    "file cache: {} turnover lines, want at least 4 — {file_series:?}\n{log}",
                    file_series.len()
                ));
            }
            if block_series.len() < 4 {
                return Err(format!(
                    "block cache: {} turnover lines, want at least 4 — {block_series:?}\n{log}",
                    block_series.len()
                ));
            }
            // The file cache's bound is the derivation, not an absolute:
            // eviction never takes a dirty page and gives up only when
            // everything resident is dirty, so an over-budget sample is
            // lawful exactly when its own line says dirty == resident — a
            // clean overage still reds. The guest stages the overage on
            // every run, and every episode must close with a sample back
            // within the bound (the kernel prints the close unconditionally).
            let mut over_samples = 0usize;
            for &(evictions, resident, dirty) in &file_series {
                if resident > file_budget {
                    over_samples += 1;
                    if dirty != resident {
                        return Err(format!(
                            "file cache: {resident} entries resident against a {file_budget} \
                             bound after {evictions} evictions with only {dirty} dirty — the \
                             overage is not the un-flushed working set, so eviction failed to \
                             take a clean page it was allowed to:\n{log}"
                        ));
                    }
                }
            }
            if over_samples == 0 {
                return Err(format!(
                    "the staged all-dirty overage never printed an over-budget sample, so the \
                     budget's one declared escape ran unobserved:\n{log}"
                ));
            }
            let last_over = file_series
                .iter()
                .rposition(|&(_, resident, _)| resident > file_budget)
                .expect("over_samples > 0 was checked above");
            if !file_series[last_over + 1..].iter().any(|&(_, r, _)| r <= file_budget) {
                return Err(format!(
                    "file cache: the last over-budget sample is never followed by one back \
                     within the bound, after every writer flushed — the overage outlived its \
                     excuse:\n{log}"
                ));
            }
            for &(evictions, resident) in &block_series {
                if resident > block_budget {
                    return Err(format!(
                        "block cache: {resident} entries resident against a {block_budget} bound \
                         after {evictions} evictions — the bound does not hold:\n{log}"
                    ));
                }
            }
            if file_series[file_series.len() - 1].0 <= file_series[0].0 {
                return Err(format!("file cache: eviction count never advanced: {file_series:?}"));
            }
            if block_series[block_series.len() - 1].0 <= block_series[0].0 {
                return Err(format!("block cache: eviction count never advanced: {block_series:?}"));
            }

            eprintln!(
                "  [cache] file {} evictions over {} turnovers ({over_samples} lawful all-dirty \
                 over-budget sample(s)), block {} evictions over {}; clean residency never above \
                 {file_budget}/{block_budget}",
                file_series[file_series.len() - 1].0,
                file_series.len(),
                block_series[block_series.len() - 1].0,
                block_series.len()
            );
            Ok(())
        }
        "xhci_slot_exhaustion" => {
            // A device count is untrusted input: more devices than the driver
            // has room for must cost those devices and nothing else. QEMU
            // cannot stage it — see XHCI_WIDE for why `slots=` is not the
            // actuator it looks like — so the kernel clamps itself to one
            // device block and the six-device bus does the rest. QEMU's Enable
            // Slot ignores MaxSlotsEn too, so the slot ids the controller hands
            // back really do run past the pool: this drives the driver's own
            // bound, not the controller's politeness.
            let options = BootOptions {
                profile: qemu::Profile::MetalUsb,
                kernel_params: &["xhci-one-slot"],
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            let usb = usb_argv(&argv);
            if usb.len() < 3 {
                return Err(format!("nothing to overflow with: {usb:?}"));
            }

            let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let log = qemu.boot_log().to_string();

            let Some(dma) = parse_xhci_layout(&log) else {
                return Err(format!("the driver printed no DMA layout line:\n{log}"));
            };
            if dma.blocks != 1 {
                return Err(format!("device blocks={}, want exactly 1: {dma:?}", dma.blocks));
            }
            // And it is the feature that bound it. A build where the ceiling
            // stopped reaching `Layout::new` reports the controller's own 64
            // here and drops nothing, which is a green test with no shortage
            // in it.
            if dma.cap_slots <= dma.blocks {
                return Err(format!(
                    "max_slots={} — there is no shortage to observe: {dma:?}",
                    dma.cap_slots
                ));
            }

            // Every device past the first is dropped, one line each.
            let slots = parse_xhci_slots(&log);
            let over = log.matches("beyond the pool").count();
            if over != usb.len() - 1 {
                return Err(format!(
                    "{over} devices dropped for want of a block, want {} (slots {slots:?}):\n{log}",
                    usb.len() - 1
                ));
            }
            if slots != [1] {
                return Err(format!("slots {slots:?} got a block, want just slot 1:\n{log}"));
            }
            // And every one of them gave its slot straight back. A slot is the
            // controller's from the moment Enable Slot answers, so a device
            // refused and left plugged in used to keep one for the life of the
            // boot — which is this test's own bus five times over, on a
            // controller the shortage is staged on.
            let given_back = log.matches("disabled").count();
            if given_back != over {
                return Err(format!(
                    "{given_back} slot(s) disabled for {over} refused device(s):\n{log}"
                ));
            }

            // The one device that did get the block was enumerated to
            // completion, which is what makes "the extra devices and nothing
            // else" more than the absence of a panic. On this bus that device
            // is the boot stick — QEMU puts it on the controller's first
            // SuperSpeed port register, ahead of every USB2 one — so what it
            // proves is block 0's output context, its EP0 ring and its bulk
            // pair, not a HID's interrupt ring. A `dev_base` that overlapped
            // the shared head would put slot 1's device context on the command
            // ring and the next command would fail here.
            for bad in [
                "Enable Slot failed",
                "Address Device failed",
                "GET_DESCRIPTOR",
                "Configure Endpoint failed",
                "not enabled after reset",
            ] {
                if log.contains(bad) {
                    return Err(format!("{bad:?} on the one device that fit:\n{log}"));
                }
            }
            if !log.contains("xHCI: device addressed") {
                return Err(format!("slot 1 got a block and was never addressed:\n{log}"));
            }
            // And it was driven all the way to a disk. The device blocks are
            // what ran short, not the mass-storage blocks, so the one device
            // that fit has to come out the far end with a capacity.
            if log.matches("usb-storage: disk ").count() != 1 {
                return Err(format!("the stick that fit did not bind as a disk:\n{log}"));
            }
            if !log.contains("usb-storage: 1 device(s)") {
                return Err(format!("want exactly one disk, the stick that fit:\n{log}"));
            }
            serial::Serial::named("boot console", log.as_str()).must_be_clean()?;
            eprintln!(
                "  [xhci] 1 block of {} for {} devices, {over} dropped, slot 1 addressed",
                dma.stride,
                usb.len()
            );
            Ok(())
        }
        "irq_census_conservation" => {
            // Four CPUs, because both halves of this test are vacuous on one.
            // The census has to be able to *say* an AP took an interrupt before
            // "every device interrupt is cpu0's" means anything.
            let options = BootOptions { smp: 4, ..BootOptions::default() };
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();
            // Two processes, so the run carries at least two censuses per CPU
            // and their monotonicity is checkable. The second munmaps a live
            // mapping — the cheapest counted shootdown on a 4-CPU guest, which
            // is what gives the issuer-side check below a non-zero subject.
            let first = qemu.run_test("echo one", Duration::from_secs(30));
            let second = qemu.run_test("test_rs_std_mmap", Duration::from_secs(30));
            irq_census(&format!(
                "{boot}\n{}\n{}\n{}\n{}",
                first.before, first.serial, second.before, second.serial
            ))
        }
        "ioapic_topology" => {
            // Everything the I/O APIC driver says happens in Phase 2, long
            // before the virtio-console exists, so the 16550 file is where a
            // host reads it. On the T14 the same lines are kernel records and
            // reach the stick, which is what lets one predicate judge both.
            let qemu = QemuInstance::boot(test_config, c_bins, rust_bins);
            // The ready marker only proves the guest booted; the lines under
            // test were written before that, so nothing else to wait for.
            ioapic_topology(qemu.boot_log())
        }
        "control_regs" => {
            const CPUS: u32 = 4;
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions { smp: CPUS, ..Default::default() },
            );
            control_regs(qemu.boot_log(), CPUS)
        }
        "control_regs_negative" => control_regs_negative(test_config, c_bins, rust_bins),
        "guest_dies_with_its_harness" => common::orphan::guest_dies_with_its_harness(test_config),
        "smp_roster_and_tsc_trail" => {
            // Eight, which is the T14's own count and this suite's ceiling.
            const CPUS: u32 = 8;
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions { smp: CPUS, ..Default::default() },
            );
            smp_roster_and_tsc_trail(qemu.boot_log(), CPUS)
        }
        "pmm_accounting" => {
            let qemu = QemuInstance::boot(test_config, c_bins, rust_bins);
            pmm_accounting(qemu.boot_log())
        }
        "root_from_memory" => {
            let qemu = QemuInstance::boot(test_config, c_bins, rust_bins);
            root_from_memory(qemu.boot_log())
        }
        "boot_from_power_on" => {
            let qemu = QemuInstance::boot(test_config, c_bins, rust_bins);
            // The loader speaks on the firmware's serial, the kernel on the console.
            boot_from_power_on(&format!("{}{}", qemu.uart_log(), qemu.boot_log()))
        }
        "root_withheld_refused" => {
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &[ROOT_WITHHELD_PARAM],
                    ready_marker: ROOT_WITHHELD_REFUSAL,
                    ..Default::default()
                },
            );
            // Both channels: this kernel dies before virtio-console init.
            root_withheld_refused(&format!("{}{}", qemu.boot_log(), qemu.uart_log()))
        }
        "kernel_args_layout_refused" => {
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &[LAYOUT_ZERO_PARAM],
                    ready_marker: LAYOUT_REFUSAL,
                    ..Default::default()
                },
            );
            // Both channels: this kernel dies before virtio-console init.
            kernel_args_layout_refused(&format!("{}{}", qemu.boot_log(), qemu.uart_log()))
        }
        "acpi_table_inventory" => {
            let qemu = QemuInstance::boot(test_config, c_bins, rust_bins);
            acpi_table_inventory(qemu.boot_log())
        }
        "timer_calibration" => {
            let qemu = QemuInstance::boot(test_config, c_bins, rust_bins);
            timer_calibration(qemu.boot_log())
        }
        "pci_inventory" => {
            let qemu = QemuInstance::boot(test_config, c_bins, rust_bins);
            pci_inventory(qemu.boot_log())
        }
        "smp_failed_ap_leaves_no_hole" => {
            smp_failed_ap_leaves_no_hole(test_config, c_bins, rust_bins)
        }
        "input_merge" => {
            // The check runs in the kernel and panics on mismatch, so a
            // failure arrives as a dead boot; the marker is the only proof it
            // ran at all.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["test-input-merge"],
                    ..Default::default()
                },
            );
            input_merge_ok(qemu.boot_log())
        }
        "i8042_health" => {
            // The failure mode that had no line at all: `init` arms the pin,
            // prints its green line, and nothing ever asserts. Two boots,
            // because the transition is the claim and one boot can only be on
            // one side of it — the first is never touched, the second is.
            //
            // Boot one waits on the verdict *as its ready marker*, so a driver
            // that never reaches it fails as a boot timeout naming the line it
            // waited for.
            let quiet_boot = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    ready_marker: "the pin has never asserted",
                    ..Default::default()
                },
            );
            let quiet_log = quiet_boot.boot_log().to_string();
            let Some(quiet) = quiet_log.lines().find(|l| l.contains("the pin has never asserted"))
            else {
                return Err(format!("no quiet verdict:\n{quiet_log}"));
            };
            // The counters, not the sentence.
            if !quiet.contains("0 interrupts") {
                return Err(format!("the quiet verdict does not say it saw none: {quiet}"));
            }
            // And nothing on this machine claimed the pin asserts, on a boot
            // where nothing touched the keyboard. A report that printed both
            // lines unconditionally would satisfy every search below.
            if let Some(wrong) = quiet_log.lines().find(|l| l.contains("the pin asserts")) {
                return Err(format!("the pin asserted with nothing to assert it: {wrong}"));
            }
            // Nor its mute twin, which is reached from the same `irqs > 0` gate
            // and would otherwise be a second line free to print on every boot.
            if let Some(wrong) = quiet_log.lines().find(|l| l.contains("nothing decoded")) {
                return Err(format!("bytes decoded to nothing with no bytes at all: {wrong}"));
            }
            drop(quiet_boot);

            // Boot two: the same kernel, one keystroke.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() },
            );
            if !qemu.boot_log().contains("i8042: kbd set2+xlat") {
                return Err(format!("the PS/2 keyboard never came up:\n{}", qemu.boot_log()));
            }
            let result = qemu.run_test_hooked(
                "test_rs_i8042_keyboard",
                Duration::from_secs(20),
                I8042_READY,
                |socket| {
                    qemu::qmp_send_keys(socket, &[("a", true), ("a", false)]);
                    thread::sleep(Duration::from_millis(100));
                    send_i8042_sentinel(socket);
                },
            );
            if let Some(err) = &result.error {
                return Err(format!("{err}\n{}", result.stdout));
            }
            let Some(line) = result.serial.lines().find(|l| l.contains("the pin asserts")) else {
                return Err(format!(
                    "a key was injected and the driver never said the pin asserts:\n{}",
                    result.serial
                ));
            };
            let words: Vec<&str> = line.split_whitespace().collect();
            let field = |name: &str| -> Option<u64> {
                let at = words.iter().position(|w| w.trim_end_matches(',') == name)?;
                words.get(at.checked_sub(1)?)?.parse().ok()
            };
            let irqs = field("interrupts")
                .ok_or_else(|| format!("unreadable health line: {line}"))?;
            let bytes = field("bytes").ok_or_else(|| format!("unreadable health line: {line}"))?;
            // The chain the line claims, end to end: the pin asserted, the ISR
            // read the port, and the decoder produced an event. Interrupts
            // alone would go green on a driver whose ring never filled.
            let keys = field("keys").ok_or_else(|| format!("unreadable health line: {line}"))?;
            if irqs == 0 || bytes == 0 || keys == 0 {
                return Err(format!(
                    "the alive line reports {irqs} interrupts, {bytes} bytes, {keys} keys: {line}"
                ));
            }

            // Boot three: the arming edge staged — the vector delivered once
            // with no byte behind it, because init consumed the byte itself.
            // The quiet verdict must stand saying so; a driver that reads the
            // edge as the machine's never prints this boot's ready marker.
            drop(qemu);
            let staged_boot = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    kernel_params: &["i8042-arm-edge"],
                    ready_marker: "the pin has never asserted",
                    ..Default::default()
                },
            );
            let staged_log = staged_boot.boot_log().to_string();
            let Some(staged) =
                staged_log.lines().find(|l| l.contains("the pin has never asserted"))
            else {
                return Err(format!("no staged quiet verdict:\n{staged_log}"));
            };
            if !staged.contains("the arming edge") {
                return Err(format!(
                    "the staged empty delivery was not attributed to the arming: {staged}"
                ));
            }
            if let Some(wrong) =
                staged_log.lines().find(|l| l.contains("no byte behind any of them"))
            {
                return Err(format!("the arming edge was read as the machine's: {wrong}"));
            }
            drop(staged_boot);

            eprintln!("  [i8042] {}", quiet.trim());
            eprintln!("  [i8042] {}", line.trim());
            eprintln!("  [i8042] {}", staged.trim());
            Ok(())
        }
        "operation_nesting" => {
            // **An inner `scheduler::Operation` may only narrow, and its drop
            // restores what it displaced.** `Operation::begin` stores
            // `outer.min(until)`, so a caller cannot buy itself more device
            // time by starting a second operation inside the first — which is
            // the failure `block::OPERATION` exists to stop, arriving one layer
            // lower.
            //
            // Nothing host-side can read it: the type reaches `percpu::cpu_id`
            // and `driver::current_handle`, and `kernel/` is excluded from the
            // host workspace, so a `Operation` cannot be constructed off a
            // booted machine. The other gates that drive an establishment —
            // `cache_eviction`'s `nvme-spent-budget`, `usb_storage_gate`'s —
            // prove a narrowing happened by the refusal it produces and read
            // none of the values, and all of them establish from a boot phase,
            // which is the *task-less* slot. `kernel/src/sched_gate.rs` runs
            // three nested establishments with known deadlines in both homes
            // and prints what every level saw.
            //
            // **The bound is derived here and not read off the kernel's
            // verdict**: the kernel prints what each level *asked* for and what
            // it *observed*, and this recomputes the running minimum. A kernel
            // that printed a verdict would be marking its own paper.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["sched-operation-nesting"],
                    ..Default::default()
                },
            );
            operation_nesting_log(qemu.boot_log())
        }
        "leak_rollback_selftest" => {
            // Two "acquire before a fallible step" controls run in the kernel at
            // boot: each prints PASS only when the in-tree count returned to its
            // baseline after a refused call; reverting either fix prints FAIL.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["leak-rollback-selftest"],
                    ..Default::default()
                },
            );
            leak_rollback(qemu.boot_log())
        }
        "process_reopen_selftest" => {
            // The kernel reopens init by pid after the only handle to it has gone; on
            // the `sealed` row that install took the boot down, so a guest that never
            // reaches the verdict line is the red.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["process-reopen-selftest"],
                    ..Default::default()
                },
            );
            process_reopen(qemu.boot_log())
        }
        "driver_wait_refused" => {
            // The actuators blind CSTS.RDY and DEVICE_STATUS, staging a
            // controller that never answers; the boot must come up naming the
            // refused register — on the unbounded shape it never reaches ready.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["nvme-rdy-stuck", "virtio-reset-stuck"],
                    ..Default::default()
                },
            );
            let log = qemu.boot_log().to_string();
            let Some(nvme) = log.lines().find(|l| l.contains("NVMe: NOT INITIALISED")) else {
                return Err(format!("the stuck NVMe was never refused by name:\n{log}"));
            };
            if !nvme.contains("CSTS.RDY would not set in") {
                return Err(format!("the NVMe refusal does not name the register: {nvme}"));
            }
            let virtio = log
                .lines()
                .filter(|l| l.contains("did not zero DEVICE_STATUS for its reset"))
                .count();
            if virtio == 0 {
                return Err(format!("no stuck virtio device was refused by name:\n{log}"));
            }
            eprintln!("  [waits] {}", nvme.trim());
            eprintln!(
                "  [waits] {virtio} virtio device(s) refused on the reset budget; the boot \
                 came up without them"
            );
            Ok(())
        }
        "read_fault_selftests" => {
            // Three kernel-boot controls: a backing read after deletion is
            // refused on both writable mounts, and a page-cache slot whose fill
            // the device refused is unbound. Reverting either prints FAIL.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["revoked-backing-selftest", "pc-unbind-selftest"],
                    ..Default::default()
                },
            );
            read_fault_probes(qemu.boot_log())
        }
        "lapic_spurious_vector" => {
            // `apic::enable_x2apic` writes 0xFF into the SVR, a vector the IDT
            // must gate or the CPU escalates to `#DF`; the SDM's classic
            // condition needs a TPR write this kernel never makes, so it raises
            // the vector on purpose. The second parameter stages the same fault
            // one gate over — a vector no row claims — which the catch-all must
            // count, remember and acknowledge; the base without it dies `#DF`.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["lapic-spurious-selftest", "unclaimed-vector-selftest"],
                    ..Default::default()
                },
            );
            lapic_vectors(qemu.boot_log())
        }
        "panic_halts_the_others_first" => panic_halts_the_others_first(test_config, c_bins, rust_bins),
        "hda_two_live_refused" => hda_two_live_refused(test_config, c_bins, rust_bins),
        "virtio_used_ring" => {
            // Both fields of a virtqueue used-ring element are written by the
            // device, and on virtio-sound's control and event queues the ring
            // is inside a page a userland process maps writable. Every virtio
            // device QEMU implements writes correct elements and no device or
            // machine property makes one report a head descriptor it was never
            // given, so a boot certifies the correct case and nothing else.
            // The driver therefore runs the shipped `poll_used` over eleven
            // crafted elements at init under this parameter — a real queue on a
            // real DMA page, with the kernel writing the ring where the device
            // would.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["virtio-used-selftest"],
                    ..Default::default()
                },
            );
            let log = qemu.boot_log().to_string();
            if let Some(bad) = log.lines().find(|l| l.contains("used-ring selftest FAILED")) {
                return Err(format!("{bad}\n{log}"));
            }
            let Some(verdict) = log.lines().find(|l| l.contains("used-ring selftest")) else {
                return Err(format!("the parse's self-test never ran:\n{log}"));
            };
            // `11/11`, not "no failures": a self-test that ran zero cases would
            // satisfy the absence of a FAILED line just as well. Four of the
            // eleven are elements `poll_used` must *accept*, and the eleventh
            // is `refused == 7`, so this one number pins both directions.
            if !verdict.contains("11/11") {
                return Err(format!("not every used-ring element was parsed as required: {verdict}"));
            }
            // Once for the machine. It touches no device, so a run per virtio
            // driver would be four verdicts about the same eleven elements.
            let ran = log.matches("used-ring selftest").count();
            if ran != 1 {
                return Err(format!("the self-test ran {ran} times, wanted once\n{log}"));
            }
            // The one wait on a used ring: a completion found only after the
            // bound, as a waiter off its CPU for all of it finds one, is
            // taken, and a device that never answers is not.
            if !log.lines().any(|l| l.contains("virtio: wait selftest 2/2")) {
                return Err(format!("the wait's self-test did not pass both cases:\n{log}"));
            }
            eprintln!("  [virtio] {}", verdict.trim());
            Ok(())
        }
        "pci_capability_walk" => {
            // A capability list is the device's, and QEMU publishes only
            // well-formed ones, so the kernel drives crafted layouts at init
            // under this parameter.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    kernel_params: &["pci-cap-selftest"],
                    ..Default::default()
                },
            );
            pci_cap_selftest(qemu.boot_log())
        }
        "shipped_config_boots" => {
            // The config the project ships, booted rather than read: `cargo
            // run`'s machine is the owner's desktop, so nothing else
            // exercises the shipped init list or its program namespaces — a
            // change to `system.toml` alone could red no suite test in
            // either direction. `Gop` is `cargo run -- --gop`'s shape, the
            // GOP framebuffer plus the whole virtio block, so every daemon in
            // `[boot] start` finds its device. No test binaries go into the
            // ROOT: the image booted here is the image shipped.
            let config = Path::new(env!("CARGO_MANIFEST_DIR"));
            let mut qemu = QemuInstance::boot_with_options(
                config,
                &[],
                &[],
                BootOptions {
                    profile: qemu::Profile::Gop,
                    ready_marker: "Boot: complete",
                    ..Default::default()
                },
            );
            let mut log = qemu.boot_log().to_string();
            // Each daemon's own announcement, not only init's spawn line: a
            // daemon that started and then failed its device claim would
            // satisfy the spawn lines alone. filepicker is the one starter
            // with no startup line — its `init: started` row is its whole
            // witness here.
            for marker in [
                "compositor: ready",
                "netd: ready",
                "soundd: ready",
                "logd: this boot's kernel log is",
            ] {
                qemu::await_marker(&mut qemu, &mut log, marker, marker)?;
            }
            let start = toyos_build::build::boot_start(&config.join("system.toml"));
            for program in &start {
                let line = format!("init: started {program}");
                qemu::await_marker(&mut qemu, &mut log, &line, &line)?;
            }
            serial::Serial::named("boot console", log.as_str()).must_be_clean()?;
            eprintln!(
                "  [shipped] the shipped system.toml boots: {} started, four daemons ready",
                start.join(", ")
            );
            Ok(())
        }
        "query_pci_agreement" => {
            // What QEMU was told to create against what the guest enumerated,
            // as two whole sets: `info pci` is the device model's own account
            // of the bus, the kernel's ECAM walk is the guest's, and every
            // earlier profile claim was checked only against the harness's
            // argv — the same source it would be verifying. Metal, because
            // the machine whose device set is not the harness's choice is the
            // shape this instrument exists for.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    qmp: true,
                    ..Default::default()
                },
            );
            let log = qemu.boot_log().to_string();
            let Some(complete) = log.lines().find(|l| l.contains("PCI: Enumeration complete"))
            else {
                return Err(format!("PCI enumeration did not complete on this boot\n{log}"));
            };
            let guest = guest_pci_functions(&log)?;
            // The census line guards the parse: a reworded device line would
            // otherwise shrink the set this comparison reads.
            if !complete.contains(&format!("{} functions", guest.len())) {
                return Err(format!(
                    "the guest declared {complete:?} and this parse found {} device lines",
                    guest.len()
                ));
            }
            let answer = {
                let mut monitor = qemu::QmpMonitor::open(qemu.qmp_socket());
                monitor.human("info pci")
            };
            drop(qemu);
            let host = qmp_pci_functions(&answer)?;
            if guest != host {
                let missing: Vec<String> =
                    host.difference(&guest).map(describe_pci_function).collect();
                let invented: Vec<String> =
                    guest.difference(&host).map(describe_pci_function).collect();
                return Err(format!(
                    "the guest's enumeration and QEMU's own account of the bus disagree — \
                     QEMU has {} function(s) the guest never decoded [{}] and the guest \
                     decoded {} [{}] QEMU does not claim\ninfo pci:\n{answer}\n{log}",
                    missing.len(),
                    missing.join(", "),
                    invented.len(),
                    invented.join(", "),
                ));
            }
            eprintln!(
                "  [pci] the guest and QEMU agree on all {} functions of the Metal bus",
                guest.len()
            );
            Ok(())
        }
        "xhci_descriptor_walk" => {
            // A configuration descriptor is the device's, and a device is not
            // kernel code. Every device QEMU can attach describes itself
            // correctly, so a boot certifies that the parser handles a correct
            // descriptor and nothing else — while the interesting inputs are
            // the wrong ones, and one of them is an endpoint address naming
            // endpoint 0, whose device context index is the slot context or
            // EP0's. The parser is pure, so the driver runs it over nine
            // crafted descriptors at init under this feature.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    kernel_params: &["xhci-descriptor-selftest"],
                    ..Default::default()
                },
            );
            xhci_descriptors(qemu.boot_log())
        }
        "xhci_xecp_walk" => {
            // The xHCI extended-capability list is firmware's, and firmware is
            // not kernel code. QEMU's controller publishes a list with no USB
            // Legacy Support capability in it, so a boot certifies exactly one
            // thing: the walk runs on a real controller and terminates. Every
            // way the list can be *wrong* — a pointer out of the register
            // window, a chain that never ends, a window reading all ones — is
            // a shape no controller in reach produces, so the driver walks
            // eight of them at init under this feature and says how many it
            // refused.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    kernel_params: &["xhci-xecp-selftest"],
                    ..Default::default()
                },
            );
            xhci_xecp(qemu.boot_log())
        }
        "i8042_budget_expiry" => {
            // The arithmetic defect this feature stages: stage budgets summing
            // past the total they clamp to. With the total spent before the
            // probe starts, every wait below returns immediately on a
            // controller that is answering perfectly — which is what a slow EC
            // looks like from inside the driver, and what used to surface as
            // `DISABLED — cfg … did not take`, a controller fault.
            let qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    kernel_params: &["i8042-budget-expired"],
                    ..Default::default()
                },
            );
            let log = qemu.boot_log().to_string();
            let Some(line) = log.lines().find(|l| l.contains("init budget")) else {
                return Err(format!(
                    "the budget was spent before the probe began and nothing said so:\n{log}"
                ));
            };
            // Naming the stage is the whole point: "it timed out" is not a
            // diagnosis on a machine that cannot be single-stepped.
            const STAGES: &[&str] = &["self-test", "keyboard", "aux reset", "the pin could be armed"];
            if !STAGES.iter().any(|s| line.contains(s)) {
                return Err(format!(
                    "a budget expiry that does not name what ran out: {line}"
                ));
            }
            // And it must not still be wearing a controller fault's clothes.
            if let Some(wrong) = log.lines().find(|l| l.contains("did not take")) {
                return Err(format!(
                    "a timeout still reports as a controller fault: {wrong}"
                ));
            }
            // Losing the keyboard must not cost the boot.
            if boot_millis(&log).is_none() {
                return Err(format!("the boot did not finish:\n{log}"));
            }
            eprintln!("  [i8042] {}", line.trim());
            Ok(())
        }
        "i8042_fadt_denial" => {
            // The T14's verdict, reproduced: firmware says there is no 8042 and
            // there is one. `i8042-fadt-denial` hands the probe the laptop's own
            // FADT answer — revision 6, iapc_boot_arch=0x0011 — on QEMU's
            // working controller, because QEMU cannot stage the disagreement
            // itself: it derives the bit from the presence of the device.
            //
            // Delivery to userland is the assertion, not the log line. "The
            // driver attached" is what a gate removal is supposed to produce;
            // "the keys arrive" is what it is *for*, and only the second one
            // fails if some later step believes the claim instead.
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                kernel_params: &["i8042-fadt-denial"],
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();
            // Revision 6 is what proves the substitution took: QEMU's own FADT
            // is revision 3, so this line cannot be the machine's.
            let want_claim = "FADT rev 6 iapc_boot_arch=0x0011, bit 1 (8042) clear";
            let Some(claim) = boot.lines().find(|l| l.contains(want_claim)) else {
                return Err(format!("the probe was never handed a denial:\n{boot}"));
            };
            if !boot.contains("i8042: kbd set2+xlat (readback 0x41)") {
                return Err(format!(
                    "firmware denied the controller and the driver believed it:\n{boot}"
                ));
            }
            let result = qemu.run_test_hooked(
                "test_rs_i8042_keyboard",
                Duration::from_secs(20),
                I8042_READY,
                |socket| {
                    for key in ["h", "e", "l", "l", "o"] {
                        qemu::qmp_send_keys(socket, &[(key, true), (key, false)]);
                        thread::sleep(Duration::from_millis(20));
                    }
                    send_i8042_sentinel(socket);
                },
            );
            if let Some(err) = &result.error {
                return Err(format!("{err}\n{}", result.stdout));
            }
            let typed: String = parse_key_events(&result.stdout)
                .iter()
                .filter(|e| e.modifiers & 0x10 == 0)
                .map(|e| e.translated.as_str())
                .collect();
            if !typed.contains("hello") {
                return Err(format!(
                    "typed {typed:?} — the keyboard firmware denied does not reach userland"
                ));
            }
            eprintln!("  [i8042] {}", claim.trim());
            eprintln!("  [i8042] typed {typed:?} through a controller firmware denied");
            Ok(())
        }
        "i8042_kbd_echo" => {
            // The T14's second answer, reproduced: a healthy controller whose
            // keyboard will not report its scancode set. `i8042-kbd-echo`
            // answers the `0xF0 0x00` argument byte with `0xEE` — ECHO's own
            // reply, the byte the laptop printed — because QEMU's PS/2 keyboard
            // implements the command and nothing on the host side turns that
            // off.
            //
            // Two assertions, and the second is the one with teeth. The log
            // line proves the driver took the *assumed* branch rather than
            // reading the set: it names the byte, and its parenthetical is not
            // `readback 0x41`, so a driver that quietly kept reading the set
            // would fail here even though the keyboard works. Typing "hello"
            // through to a userland process proves the branch delivers, which
            // no log line can: a driver that logs the assumption and then
            // refuses, or that arms a pin nothing decodes, is green on the
            // first assertion alone.
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                kernel_params: &["i8042-kbd-echo"],
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();
            let want = "0xF0 0x00 answered 0xee";
            let Some(refusal) = boot.lines().find(|l| l.contains(want)) else {
                return Err(format!("the keyboard never refused the set query:\n{boot}"));
            };
            let Some(attached) =
                boot.lines().find(|l| l.contains("i8042: kbd set2+xlat (assumed,"))
            else {
                return Err(format!("the driver refused the keyboard outright:\n{boot}"));
            };
            if boot.contains("(readback 0x41)") {
                return Err(format!(
                    "the injection did not take: the driver still read the set back:\n{boot}"
                ));
            }
            let result = qemu.run_test_hooked(
                "test_rs_i8042_keyboard",
                Duration::from_secs(20),
                I8042_READY,
                |socket| {
                    for key in ["h", "e", "l", "l", "o"] {
                        qemu::qmp_send_keys(socket, &[(key, true), (key, false)]);
                        thread::sleep(Duration::from_millis(20));
                    }
                    send_i8042_sentinel(socket);
                },
            );
            if let Some(err) = &result.error {
                return Err(format!("{err}\n{}", result.stdout));
            }
            let typed: String = parse_key_events(&result.stdout)
                .iter()
                .filter(|e| e.modifiers & 0x10 == 0)
                .map(|e| e.translated.as_str())
                .collect();
            if !typed.contains("hello") {
                return Err(format!(
                    "typed {typed:?} — a keyboard that will not report its set does not reach \
                     userland"
                ));
            }
            // The TrackPoint is on the far side of the keyboard block, so a
            // refusal that returns costs the pointer too. It must not here.
            if !boot.contains("i8042: aux rate=100") {
                return Err(format!("the aux port never came up behind the refusal:\n{boot}"));
            }
            eprintln!("  [i8042] {}", refusal.trim());
            eprintln!("  [i8042] {}", attached.trim());
            eprintln!("  [i8042] typed {typed:?} on a keyboard that will not report its set");
            Ok(())
        }
        "i8042_undecoded_bytes" => {
            // The T14 said `1 interrupts, 1 bytes, 0 keys, 0 motion` and the
            // counters could not name a suspect: 84 of the 256 single byte
            // values decode to nothing under set 1, so the same arithmetic
            // covers an extended key's harmless `0xE0` prefix, a `0xAA` from a
            // keyboard that reset, a late `0xFA`, and a wire carrying raw
            // set 2. Only the byte separates them.
            //
            // Pause is the injection because it is the one key whose whole
            // sequence decodes to nothing by design — `E1 1D 45 E1 9D C5`,
            // swallowed to keep the stream in frame — so bytes-with-zero-events
            // is reproduced without depending on how the drain happens to
            // batch. Then one plain letter, which is the other half: the first
            // line must not be the last word on a keyboard that works.
            //
            // `i8042-split-burst` stages the interleaving this name's CI red
            // recorded (run 31944633004): the ISR takes four of the six bytes
            // and the mute verdict goes out with the decoder's run still open,
            // so it names nothing — on every run here, where KVM produced it
            // by scheduling luck. The verdict must then revise itself when the
            // rest of the sequence lands, which is the assertion below.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    qmp: true,
                    kernel_params: &["i8042-split-burst"],
                    ..Default::default()
                },
            );
            if !qemu.boot_log().contains("i8042: kbd set2+xlat") {
                return Err(format!("the PS/2 keyboard never came up:\n{}", qemu.boot_log()));
            }
            let result = qemu.run_test_hooked(
                "test_rs_i8042_keyboard",
                Duration::from_secs(20),
                I8042_READY,
                |socket| {
                    qemu::qmp_send_keys(socket, &[("pause", true), ("pause", false)]);
                    thread::sleep(Duration::from_millis(200));
                    qemu::qmp_send_keys(socket, &[("a", true), ("a", false)]);
                    thread::sleep(Duration::from_millis(100));
                    send_i8042_sentinel(socket);
                },
            );
            if let Some(err) = &result.error {
                return Err(format!("{err}\n{}", result.stdout));
            }
            // **Every line is read from the injection onwards**, and that is
            // not tidiness: this driver reports on its own bring-up too, and a
            // `nothing decoded` line from before the Pause was pressed is not a
            // report about the Pause. Reading the first one in the whole capture
            // is what made this test red on a line naming no byte, on the dev
            // host and on CI. The marker is the boundary the test knows,
            // because the marker is what the injection was timed off.
            let text = result.serial;
            let capture = serial::Serial::named("i8042 capture", text.as_str());
            let Some(at) = text.find(I8042_READY) else {
                return Err(format!("{I8042_READY:?} never reached the capture:\n{text}"));
            };
            let from = text[at..].find('\n').map_or(text.len(), |n| at + n + 1);
            let mut mutes = text[from..].lines().filter(|l| l.contains("nothing decoded"));
            // The staged premise first: the split put the verdict out with the
            // run still open, so the first mute line must name nothing. A first
            // line that already names bytes means the arrangement did not
            // happen and nothing below would be testing the revision.
            let blind = mutes.next().ok_or_else(|| {
                format!(
                    "bytes arrived and decoded to nothing and the driver never said so:\n{text}"
                )
            })?;
            if blind.contains("no event from") {
                return Err(format!(
                    "the staged split never beat the verdict — the first mute line already \
                     names bytes, so this run exercised nothing: {blind}"
                ));
            }
            // The revision. `0xE1` is Pause's prefix and the first byte of the
            // sequence whichever way the drain batched it; a verdict that
            // stands on the blind line — a true statement naming no suspect —
            // is the one this test exists to reject.
            let mute = mutes.find(|l| l.contains("no event from [0xe1")).ok_or_else(|| {
                format!(
                    "the verdict was said too early — {blind:?} — and never revised: no later \
                     `nothing decoded` line names the sequence:\n{text}"
                )
            })?;
            // And the picture corrects itself. A one-shot report would freeze
            // the panel on the half-arrived sequence and never say the
            // keyboard works after all — which on the T14 is a reflash.
            let alive = capture.must_say_after(I8042_READY, "the pin asserts").map_err(|why| {
                format!(
                    "a letter was typed after the undecoded bytes and the driver never \
                     revised its verdict: {why}"
                )
            })?;
            let keys = alive
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .find(|w| w[1].trim_end_matches(',') == "keys")
                .and_then(|w| w[0].parse::<u64>().ok())
                .ok_or_else(|| format!("unreadable alive line: {alive}"))?;
            if keys == 0 {
                return Err(format!("the revised verdict still decodes nothing: {alive}"));
            }
            eprintln!("  [i8042] {}", blind.trim());
            eprintln!("  [i8042] {}", mute.trim());
            eprintln!("  [i8042] {}", alive.trim());
            Ok(())
        }
        "i8042_absent" => {
            let without = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    i8042: false,
                    ..Default::default()
                },
            );
            let log = without.boot_log().to_string();
            // Measured: `-machine q35,i8042=off` also clears the FADT
            // IAPC_BOOT_ARCH 8042 bit. That used to make this test certify the
            // gate; now it is what makes it certify the opposite — firmware
            // denies the controller, the driver probes anyway, and the
            // *handshake* is what refuses. Both halves are asserted, because a
            // refusal on the right machine for the wrong reason is exactly the
            // false pass available here.
            let Some(claim) = log.lines().find(|l| l.contains("iapc_boot_arch")) else {
                return Err(format!("the driver never said what firmware claimed:\n{log}"));
            };
            if !claim.contains("bit 1 (8042) clear") {
                return Err(format!(
                    "`-machine q35,i8042=off` no longer clears the FADT bit, so this \
                     configuration no longer stages a firmware denial: {claim}"
                ));
            }
            // The floating bus, not any of the sixteen handshake refusals: on a
            // machine with nothing there the probe must cost one `inb`.
            let want = "i8042: absent — port 0x64 reads 0xff";
            if !log.contains(want) {
                return Err(format!("no `{want}` line on a machine with no i8042:\n{log}"));
            }
            if boot_millis(&log).is_none() {
                return Err(format!("no `Boot: complete` line:\n{log}"));
            }
            eprintln!("  [i8042] firmware: {}", claim.trim());
            eprintln!(
                "  [i8042] {}",
                log.lines().find(|l| l.contains(want)).unwrap_or_default().trim()
            );
            Ok(())
        }
        "i8042_quarantine" => {
            // A controller producing bytes faster than the ISR's bound can
            // drain them is the one case the bound alone still lets livelock
            // a CPU. It must cost a keyboard, not a CPU.
            let mut qemu = QemuInstance::boot_with_options(
                test_config,
                c_bins,
                rust_bins,
                BootOptions {
                    profile: qemu::Profile::Metal,
                    qmp: true,
                    kernel_params: &["i8042-fault"],
                    ..Default::default()
                },
            );
            if !qemu.boot_log().contains("i8042: fault injection armed") {
                return Err(format!(
                    "the fault was never armed — did init fail?\n{}",
                    qemu.boot_log()
                ));
            }
            // One key, and the flood behind it: what ends the wait is the
            // driver's own line.
            let socket = qemu.qmp_socket().to_path_buf();
            qemu::qmp_send_keys(&socket, &[("a", true), ("a", false)]);
            let mut serial = String::new();
            await_guest(&mut qemu, &mut serial, "the quarantine line", |c| {
                c.contains("i8042: quarantined")
            })?;
            let line = serial
                .lines()
                .find(|l| l.contains("i8042: quarantined"))
                .expect("the wait ended on this line");
            // The count the driver actually achieved, not the word "masked"
            // in a format string: a quarantine that does not take the line
            // down leaves the CPU exposed to the next flood.
            let masked: u32 = line
                .split("masked=")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("unreadable quarantine line: {line}"))?;
            if masked == 0 {
                return Err(format!("quarantined without masking any line: {line}"));
            }
            eprintln!("  [i8042] {}", line.trim());
            Ok(())
        }
        "metal_sim_window_drag" => metal_sim_window_drag(rust_bins),
        "metal_sim_hostile_clipboard" => metal_sim_hostile_clipboard(rust_bins),
        "metal_sim_pointer_churn" => {
            // The owner froze his desktop twice by plugging a mouse in and
            // pulling it out again, and the second freeze landed on the fourth
            // cycle's enumeration. The compositor holds the merged pointer's
            // handle across all of it, so every cycle is a source binding and
            // releasing underneath a claim it never made and cannot see.
            //
            // The liveness signal is `compositor: frames=`, for the reason it
            // was built: it comes from a composited frame, so its absence is a
            // desktop that stopped drawing rather than an instrument that
            // stopped counting.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/metalcase");
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                ..Default::default()
            };
            metal_sim_argv_check(&qemu::profile_argv(&options))?;

            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
            let socket = qemu.qmp_socket().to_path_buf();
            let mut console = qemu.boot_log().to_string();
            let frames = |text: &str| text.matches("compositor: frames=").count();

            // A baseline first: churn against a compositor that was never
            // drawing would be a green run proving nothing.
            let deadline = std::time::Instant::now() + qemu.budget(Duration::from_secs(20));
            while std::time::Instant::now() < deadline && frames(&console) < 1 {
                console.push_str(&qemu.drain_serial(Duration::from_millis(250)));
            }
            if frames(&console) < 1 {
                return Err(format!("the compositor never composited a frame:\n{console}"));
            }
            let before = frames(&console);

            // The owner's cadence: plugged for a second or two, unplugged for
            // about as long, over and over. His freeze came on the fourth.
            const CYCLES: usize = 8;
            const SETTLE: Duration = Duration::from_millis(400);
            for cycle in 0..CYCLES {
                let id = format!("churn{cycle}");
                // One monitor at a time — a `server` socket serves one
                // connection — so each phase opens, acts and closes.
                let mut devices = qemu::QmpDevices::open(&socket);
                devices.add("usb-mouse", "xhci.0", &id, &[]);
                drop(devices);
                console.push_str(&qemu.drain_serial(SETTLE));
                // The pointer has to be *used* between binding and unbinding.
                // A source that binds and goes is a lifecycle event the
                // compositor may never look at; a source delivering motion
                // when it goes is one the compositor is reading from, which
                // is the state the owner's machine was in every time.
                let mut input = qemu::QmpInput::open(&socket);
                for step in 0..16 {
                    let dir = if step % 2 == 0 { 12 } else { -12 };
                    input.mouse(dir, dir, None);
                }
                drop(input);
                console.push_str(&qemu.drain_serial(SETTLE));
                let mut devices = qemu::QmpDevices::open(&socket);
                devices.del(&id);
                drop(devices);
                console.push_str(&qemu.drain_serial(SETTLE));
            }

            // The churn has to have reached the guest, or this gate is a
            // twenty-second sleep with an assertion after it.
            //
            // **Waited for rather than slept for.** The three `SETTLE` drains
            // pace the *host* through one cycle; whether the guest's console has
            // caught up by the last of them is a fact about how fast the machine
            // is. On a KVM runner it had not — the last two cycles' bindings were
            // still on their way out when the count was taken, and the test read
            // six of eight as a driver that missed them (run `31246245541`).
            // The assertion is the same one; what
            // changed is that a console behind the guest costs wall clock instead
            // of a verdict.
            let bindings = |text: &str| text.matches("merges as source").count();
            let deadline = std::time::Instant::now() + qemu.budget(Duration::from_secs(20));
            while std::time::Instant::now() < deadline && bindings(&console) < CYCLES {
                console.push_str(&qemu.drain_serial(Duration::from_millis(250)));
            }
            let bound = bindings(&console);
            if bound < CYCLES {
                return Err(format!(
                    "{CYCLES} plug/unplug cycles bound {bound} pointer sources — the churn did \
                     not reach the kernel, so nothing here was tested:\n{console}"
                ));
            }

            // And the motion reached the compositor, or the churn was against
            // a pointer nobody was reading: a frame that drew the software
            // cursor is one whose damage met it, and the idle desktop's only
            // damage is the taskbar's clock, away from where the cursor starts.
            let moved = console
                .lines()
                .filter(|l| l.contains("compositor: frames="))
                .filter_map(|l| l.split(" cursor=").nth(1))
                .filter_map(|rest| rest.split_whitespace().next())
                .filter_map(|n| n.parse::<u64>().ok())
                .any(|draws| draws > 0);
            if !moved {
                return Err(format!(
                    "no composited frame drew the cursor — the injected motion never reached \
                     the compositor, so the churn was against a pointer it was not \
                     reading:\n{console}"
                ));
            }

            // Still painting, counted from here rather than from the boot: the
            // reporting interval is 2 s, so two of them cannot be satisfied by
            // frames the compositor produced before the first cycle.
            let mut after = String::new();
            let deadline = std::time::Instant::now() + qemu.budget(Duration::from_secs(20));
            while std::time::Instant::now() < deadline && frames(&after) < 2 {
                after.push_str(&qemu.drain_serial(Duration::from_millis(250)));
            }
            if frames(&after) < 2 {
                return Err(format!(
                    "{STALLED} the compositor composited {before} frame batches before {CYCLES} \
                     pointer plug/unplug cycles and {} after them — the desktop stopped:\
                     \n{console}\n--- after ---\n{after}",
                    frames(&after)
                ));
            }

            let console = format!("{console}\n{after}");
            serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
            eprintln!(
                "  [metal-sim] {CYCLES} pointer plug/unplug cycles, {bound} source bindings, \
                 desktop still compositing"
            );
            Ok(())
        }
        "layout_fresh_boot" => {
            // The layout as ruled, on a boot of its own with a blank DATA
            // volume, asked over the cable because sshd is the service that
            // starts programs: a declared shell's `HOME` is init's row answer,
            // and the judge, declared nowhere, is spawned by sshd directly with
            // the `HOME` init answered for it. Before the judge, `locale` and
            // an interactive shell write the two files a session writes.
            use common::ssh::{self, HOST};
            const MINTED: &str = "sshd: minted a new host identity at /state/sshd/host_ed25519";
            const LAYOUT: &str = "de";
            const TYPED: &str = "echo layout history";
            let (guest, console) = ssh::boot_case("tests/layoutcase", rust_bins);
            if !console.contains(MINTED) {
                return Err(format!("{MINTED:?} never reached the console:\n{console}"));
            }
            let identity = ssh::Identity::mint(ssh::KEY)?;
            let port = guest.ssh_port();

            let home = ssh::ssh_exec(HOST, port, &identity, "shell -c 'echo $HOME'")?;
            if home.stdout != b"/home/toy\n" || home.status != Some(0) {
                return Err(format!(
                    "a launched shell's HOME is {:?} (ended {:?}, stderr {:?}), not /home/toy",
                    home.stdout_text(),
                    home.status,
                    home.stderr_text()
                ));
            }
            let set = ssh::ssh_exec(HOST, port, &identity, &format!("locale {LAYOUT}"))?;
            if set.status != Some(0) || !set.stdout_text().contains("Keyboard layout set to") {
                return Err(format!(
                    "`locale {LAYOUT}` ended {:?} saying {:?} {:?}",
                    set.status,
                    set.stdout_text(),
                    set.stderr_text()
                ));
            }
            let (typed, _) =
                ssh::ssh_feed(HOST, port, &identity, "shell", format!("{TYPED}\r").as_bytes())?;
            if typed.status != Some(0) || !typed.stdout_text().contains("layout history") {
                return Err(format!(
                    "the interactive shell ended {:?} saying {:?} {:?}",
                    typed.status,
                    typed.stdout_text(),
                    typed.stderr_text()
                ));
            }
            let judge = ssh::ssh_exec(
                HOST,
                port,
                &identity,
                &format!("test_rs_layout_paths {LAYOUT} '{TYPED}'"),
            )?;
            if judge.status != Some(0) {
                return Err(format!(
                    "layout_paths ended {:?} over ssh:\n{}\n{}",
                    judge.status,
                    judge.stdout_text(),
                    judge.stderr_text()
                ));
            }
            eprintln!("  [layout] a launched shell's HOME is /home/toy; {}", judge.stdout_text().trim());
            Ok(())
        }
        "lan_dhcp_lease" => lan::lan_dhcp_lease(test_config, c_bins, rust_bins),
        "lan_lease_report" => lan::lan_lease_report(test_config, c_bins, rust_bins),
        "lan_talk" => lan::lan_talk(test_config, c_bins, rust_bins),
        "lan_mdns_answer" => common::origin::mdns(c_bins, rust_bins),
        "swap_netd" => common::swap::swap_netd(test_config, c_bins, rust_bins),
        "update_boots_the_new_kernel" => common::update::update_boots_the_new_kernel(test_config, c_bins, rust_bins),
        "update_refusals_boot_the_other_slot" => {
            common::update::update_refusals_boot_the_other_slot(test_config, c_bins, rust_bins)
        }
        "update_falls_back_from_a_dying_kernel" => {
            common::update::update_falls_back_from_a_dying_kernel(test_config, c_bins, rust_bins)
        }
        "update_hang_kills_an_unproven_image" => {
            common::update::update_hang_kills_an_unproven_image(test_config, c_bins, rust_bins)
        }
        "update_grant_refuses_a_stray_partition" => {
            common::update::update_grant_refuses_a_stray_partition(test_config, c_bins, rust_bins)
        }
        "update_floor_is_the_images_own" => {
            common::update::update_floor_is_the_images_own(test_config, c_bins, rust_bins)
        }
        "update_refused_pass_credits_no_image" => {
            common::update::update_refused_pass_credits_no_image(test_config, c_bins, rust_bins)
        }
        "lan_swap" => common::swap::lan_swap(test_config, c_bins, rust_bins),
        "swap_refusals" => common::swap::swap_refusals(test_config, c_bins, rust_bins),
        "swap_crash_rolls_back" => common::swap::swap_crash_rolls_back(test_config, c_bins, rust_bins),
        "swap_quiets_the_function" => {
            common::swap::swap_quiets_the_function(test_config, c_bins, rust_bins)
        }
        "swap_keeps_what_nothing_reset" => {
            common::swap::swap_keeps_what_nothing_reset(test_config, c_bins, rust_bins)
        }
        "swap_fault_tells_its_holder" => {
            common::swap::swap_fault_tells_its_holder(test_config, c_bins, rust_bins)
        }
        "swap_resets_the_function" => {
            common::swap::swap_resets_the_function(test_config, c_bins, rust_bins)
        }
        "swap_refused_device_fails" => {
            common::swap::swap_refused_device_fails(test_config, c_bins, rust_bins)
        }
        "swap_moved_device_fails" => {
            common::swap::swap_moved_device_fails(test_config, c_bins, rust_bins)
        }
        "swap_not_inherited" => common::swap::swap_not_inherited(test_config, c_bins, rust_bins),
        "lan_no_lease" => lan::lan_no_lease(test_config, c_bins, rust_bins),
        "https_tls13" => common::https::tls13_judge(rust_bins, common::https::VIRTIO).map(|_| ()),
        // The arming is asserted here and not on the bench above, whose claimed
        // function publishes no MSI capability at all: that assertion there is
        // green on every implementation of this kernel.
        "https_tls13_e1000e" => common::https::tls13_judge(rust_bins, common::https::E1000E)
            .and_then(|console| {
                common::iommu::armed_on_msix(
                    &serial::Serial::named("boot console", console.as_str()),
                    common::https::E1000E.claims,
                )
            }),
        "log_stream" => common::logstream::stream(common::logstream::VIRTIO, c_bins, rust_bins),
        "log_stream_e1000e" => {
            common::logstream::stream(common::logstream::E1000E, c_bins, rust_bins)
        }
        "log_stream_stalled_reader" => common::logstream::stalled_reader(c_bins, rust_bins),
        "log_program_line" => common::origin::line(c_bins, rust_bins),
        "log_program_forgery" => common::origin::forgery(c_bins, rust_bins),
        "log_after_a_refused_stop" => common::origin::refused_stop(c_bins, rust_bins),
        "log_resume_meets_its_flush" => common::origin::resume_meets_its_flush(rust_bins),
        "log_ring_keeps_the_owners_slots" => common::origin::keeps_the_owners_slots(rust_bins),
        "log_program_line_after_its_records" => common::origin::after_records(c_bins, rust_bins),
        "log_carrier_forgery" => common::origin::carrier_forgery(c_bins, rust_bins),
        "log_program_flood" => common::origin::flood(c_bins, rust_bins),
        "netd_connection_caps" => {
            // The only boot that runs netd at all. Its `main` opens the NIC
            // first and returns on `NotFound`, so metal-sim never reaches a
            // line of the daemon, and `tests/testcases` does not build netd —
            // between them a full suite run contained zero `netd:` lines and
            // the daemon's bound had no evidence behind it whatsoever.
            //
            // Same assertion design as `metal_sim_window_caps`: netd announces
            // the cap it derived, the guest measures where the refusals start
            // against the host server, and these must be the same number.
            let HostRun { result, console, .. } =
                netcase_against_host(rust_bins, "netd_caps", false, "")?;
            let Some(declared) = console
                .lines()
                .find_map(|l| l.split("netd: ready, at most ").nth(1))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
            else {
                return Err(format!(
                    "netd never said how many piped connections it would hold:\n{console}"
                ));
            };
            if declared == 0 {
                return Err("netd derived a cap of zero connections".to_string());
            }

            let Some(granted) = result
                .stdout
                .split("netd caps: ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
            else {
                return Err(format!("netd_caps printed no count:\n{}", result.stdout));
            };
            if granted != declared {
                return Err(format!(
                    "netd declared a cap of {declared} piped connections and accepted \
                     {granted} — the derivation and the enforcement disagree:\n{}",
                    result.stdout
                ));
            }
            eprintln!("  [netcase] netd cap {declared} piped connections, {granted} accepted then refused");
            Ok(())
        }
        "pci_function_is_exclusive" => {
            // `tests/netcase` declares `pci:1af4:1041` on netd *and* on
            // test-runner. A device a second process could claim is one whose
            // register window, MSI-X table and DMA grants two processes hold at
            // once, so the kernel is what has to refuse it: `src/build.rs`'s
            // config check names this config as its one exception, and this is
            // the boot that says the refusal exists.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
            let options = BootOptions {
                profile: qemu::Profile::Headless,
                ..Default::default()
            };
            if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
                return Err("this test needs a NIC and the profile has none".to_string());
            }
            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
            let mut console = qemu.boot_log().to_string();
            // Both claimants have acted once netd is up on the claim and init has
            // told test-runner it lost, which is what makes the count below the
            // settled one.
            await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up")?;
            await_marker(
                &mut qemu,
                &mut console,
                "init: test-runner: pci:1af4:1041 is already claimed",
                "init to refuse the second claim",
            )?;
            let log = serial::Serial::named("boot console", console.as_str());

            // Exactly one hand-over of that function. Two would be the defect
            // itself, and zero a boot that says nothing about exclusivity.
            let handovers =
                log.text().lines().filter(|l| l.contains("[1af4:1041] handed over on slot")).count();
            if handovers != 1 {
                return Err(format!(
                    "the NIC's function was handed over {handovers} times, and a second holder \
                     would drive the same registers, MSI-X entry and grants as the first:\n{}",
                    log.text()
                ));
            }
            // And the loser was told why, in the kernel's own word rather than
            // "no NIC": init prints the `AlreadyExists` arm of `refused`.
            log.must_say("init: test-runner: pci:1af4:1041 is already claimed")?;
            // netd is the one that got it, not merely the one that ran.
            log.must_say("netd: ready, at most ")?;
            aperture_account(&log)?;
            log.must_be_clean()?;
            eprintln!(
                "  [netcase] one PCI function, two claimants, one holder; and every \
                 assigned BAR inside the aperture firmware named"
            );
            Ok(())
        }
        "bar_placement_is_proven" => {
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
            let options = BootOptions {
                profile: qemu::Profile::Headless,
                qmp: true,
                ..Default::default()
            };
            if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
                return Err("this test needs a NIC and the profile has none".to_string());
            }
            let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
            let mut console = qemu.boot_log().to_string();
            await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up")?;
            let mtree = qemu::QmpMonitor::open(qemu.qmp_socket()).human("info mtree");
            let log = serial::Serial::named("boot console", console.as_str());

            // The memory the kernel was handed, read off its own record.
            let named = log.must_say("pcidev: firmware declared root bridge memory: ")?;
            let windows: Vec<(u64, u64)> = named
                .rsplit_once("memory: ")
                .map(|(_, rest)| rest)
                .unwrap_or_default()
                .split(", ")
                .filter_map(|w| {
                    let (base, end) = w.trim().strip_prefix("mem ")?.split_once("..")?;
                    Some((
                        u64::from_str_radix(base.trim_start_matches("0x"), 16).ok()?,
                        u64::from_str_radix(end.trim().trim_start_matches("0x"), 16).ok()?,
                    ))
                })
                .collect();

            // The runs the kernel offered that BAR, off the same boot.
            let runs: Vec<(u64, u64)> = log
                .text()
                .lines()
                .filter_map(|line| line.split_once("pcidev:   0x").map(|(_, rest)| rest))
                .filter_map(|rest| {
                    let (start, rest) = rest.split_once("..0x")?;
                    let end = rest.split_whitespace().next()?;
                    Some((u64::from_str_radix(start, 16).ok()?, u64::from_str_radix(end, 16).ok()?))
                })
                .collect();
            if runs.is_empty() {
                return Err(format!(
                    "this boot listed no run for a BAR to be offered, so nothing here is about \
                     a placement:\n{}",
                    log.text()
                ));
            }

            // Exactly one placement record for the NIC, and every fact in it.
            let placed: Vec<&str> = log
                .text()
                .lines()
                .map(str::trim)
                .filter(|l| l.contains("pcidev: PCI 00:03.0 BAR") && l.contains(" placed at "))
                .collect();
            let [record] = placed[..] else {
                return Err(format!(
                    "the NIC's BAR was placed {} time(s) and this test is about the one \
                     placement the boot makes:\n{}",
                    placed.len(),
                    log.text()
                ));
            };
            eprintln!("  [netcase] {record}");
            let number = |marker: &str| -> Option<u64> {
                let (_, rest) = record.split_once(marker)?;
                u64::from_str_radix(rest.split([' ', ';', ',']).next()?, 16).ok()
            };
            // **Read as numbers, not matched as a sentence.** A `contains` over
            // the clause would pass on a record whose address is outside the
            // window it claims, which is exactly the placement that hangs.
            let at = number(" placed at 0x")
                .ok_or_else(|| format!("{record:?} names no address"))?;
            let window = number("inside firmware's mem 0x")
                .ok_or_else(|| format!("{record:?} names no window the address is inside"))?;
            let reference = number("; its +0x")
                .ok_or_else(|| format!("{record:?} names no dword it read"))?;
            let after = number("dword answers 0x")
                .ok_or_else(|| format!("{record:?} does not say what the BAR answered"))?
                as u32;
            let signature = number("and answered 0x")
                .ok_or_else(|| format!("{record:?} does not say what the function answered"))?
                as u32;
            let was = number(" from 0x")
                .ok_or_else(|| format!("{record:?} does not say where the function was"))?;
            // **The dword read is the one this function publishes a value at.**
            // A modern virtio function opens its common configuration with a
            // selector that reads zero (virtio 1.2 §4.1.4.3), so reading the
            // BAR's first dword would settle every address against `0`.
            if reference != 4 {
                return Err(format!(
                    "the kernel settled this placement on the +{reference:#x} dword; the dword a \
                     modern virtio function answers a value of its own at is +0x4"
                ));
            }
            // **Neither value may be one an unanswered read produces.** q35
            // answers `0x00000000` where nothing claims an address and a root
            // complex answers all-ones, so a proof resting on either of them is
            // `0 == 0`.
            for (what, value) in [("where firmware put it", signature), ("at the new address", after)]
            {
                if value == 0 || value == u32::MAX {
                    return Err(format!(
                        "the function answered {value:#010x} {what}, which is what a read nobody \
                         answered comes back as, so this record settles nothing:\n{record}"
                    ));
                }
            }
            if after != signature {
                return Err(format!(
                    "the record calls {at:#x} placed and the function answers {after:#010x} there \
                     against {signature:#010x} at {was:#x}:\n{record}"
                ));
            }
            if !windows.iter().any(|(base, end)| *base == window && at >= *base && at < *end) {
                return Err(format!(
                    "the BAR went to {at:#x} and the record calls that inside the window at \
                     {window:#x}; the windows this boot's firmware declared are {windows:x?}"
                ));
            }
            // And the address came out of a run this boot itself printed.
            if !runs.iter().any(|(start, end)| at >= *start && at < *end) {
                return Err(format!(
                    "the BAR went to {at:#x} and the runs this boot offered are {runs:x?}"
                ));
            }
            // **And the emulator answers for the same address**: the kernel's
            // printed one is checked against where QEMU maps that function's
            // common configuration rather than believed.
            // One address, however many address spaces it appears in: QEMU
            // prints the region once per space that reaches it.
            let mut mapped: Vec<u64> = mtree
                .lines()
                .filter(|line| line.contains("virtio-pci-common-virtio-net"))
                .filter_map(|line| {
                    let (start, _) = line.trim().split_once('-')?;
                    u64::from_str_radix(start, 16).ok()
                })
                .collect();
            mapped.sort_unstable();
            mapped.dedup();
            let [common] = mapped[..] else {
                return Err(format!(
                    "`info mtree` maps this function's common configuration {} time(s), so this \
                     boot has no account of its own routing to check the kernel against:\n{mtree}",
                    mapped.len()
                ));
            };
            if common != at {
                return Err(format!(
                    "the kernel says the BAR went to {at:#x} and QEMU maps that function's \
                     registers at {common:#x}:\n{mtree}"
                ));
            }
            // And the placement is what the hand-over rests on: the same boot
            // must have handed the function over, or the record above is about
            // a BAR that moved for nothing.
            let over = log.must_say("[1af4:1041] handed over on slot")?;
            let slot = over
                .split_once("handed over on slot ")
                .and_then(|(_, rest)| rest.split(',').next())
                .ok_or_else(|| format!("unparseable hand-over record: {over:?}"))?;
            let spoke = log.must_say(&format!("pcidev: slot {slot} took its first message"))?;
            log.must_be_clean()?;
            eprintln!(
                "  [netcase] {at:#x} is inside firmware's {window:#x}, inside a run this boot \
                 printed and where QEMU itself maps that function's registers, and its \
                 +{reference:#x} dword answers {after:#010x} there as it does at {was:#x}"
            );
            eprintln!("  [netcase] {}", spoke.trim());
            Ok(())
        }
        "inspect_reads_its_owners" => {
            let mut qemu = common::inspect::boot(rust_bins)?;
            common::inspect::reads_its_owners(&mut qemu)
        }
        "netd_listener_forgery" => {
            // The netcase boot (the only NIC-under-netd one). The client binds
            // a piped listener, forges the reader-closed flag with its reader
            // open, and asserts the listener survived — netd asking the kernel,
            // not the forgeable bit. On the pre-fix netd the guest panics.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
            let bins: Vec<(String, Vec<u8>)> = rust_bins
                .iter()
                .filter(|(name, _)| name == "netd_listener_forgery")
                .cloned()
                .collect();
            if bins.is_empty() {
                return Err("netd_listener_forgery was not built".to_string());
            }
            let options = BootOptions {
                profile: qemu::Profile::Headless,
                ..Default::default()
            };
            if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
                return Err("this test needs a NIC and the profile has none".to_string());
            }
            let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
            let mut console = qemu.boot_log().to_string();
            let _ = await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up");
            let result = qemu.run_test("test_rs_netd_listener_forgery", Duration::from_secs(60));
            if let Some(err) = &result.error {
                return Err(format!("{err}\n{}", result.stdout));
            }
            if result.exit_code != Some(0) {
                return Err(format!(
                    "netd_listener_forgery exited {:?}:\n{}",
                    result.exit_code, result.stdout
                ));
            }
            if !result.stdout.contains("listener survived a forged reader-closed flag") {
                return Err(format!("no survival line from the guest:\n{}", result.stdout));
            }
            eprintln!("  [netcase] a piped listener survived a forged reader-closed flag");
            Ok(())
        }
        "netd_slow_reader" => netd_slow_reader(rust_bins),
        "netd_refused_pipes" => netd_refused_pipes(rust_bins),
        "netd_refused_accept" => netd_refused_accept(rust_bins),
        "netd_held_open" => netd_held_open(rust_bins),
        "netd_udp_refused" => netd_udp_refused(rust_bins),
        "netd_udp_any_address" => netd_udp_any_address(rust_bins),
        "dns_resolve" => dns_resolve(),
        "netd_lookup_let_go" => netd_lookup_let_go(rust_bins),
        "netd_seeds_its_stack" => netd_seeds_its_stack(),
        "netd_hostile_peer" => {
            // The netcase boot again, and for the same reason: netd's `main`
            // returns on a machine with no NIC, so this is the only config
            // where there is a daemon to be hostile to.
            //
            // The guest carries whether netd answered. The host carries the
            // half the guest cannot see: whether netd *named* what it got rid
            // of. A daemon that drops clients silently is one this machine
            // cannot be asked about afterwards, which is the whole argument for
            // the log lines.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
            let bins: Vec<(String, Vec<u8>)> = rust_bins
                .iter()
                .filter(|(name, _)| name == "netd_hostile_peer")
                .cloned()
                .collect();
            if bins.is_empty() {
                return Err("netd_hostile_peer was not built".to_string());
            }
            let options = BootOptions {
                profile: qemu::Profile::Headless,
                ..Default::default()
            };
            if !qemu::profile_argv(&options).iter().any(|a| a.contains("virtio-net")) {
                return Err("this test needs a NIC and the profile has none".to_string());
            }

            let mut qemu = QemuInstance::boot_with_options(&config, &[], &bins, options);
            let mut console = qemu.boot_log().to_string();
            let _ = await_marker(&mut qemu, &mut console, "netd: ready, at most ", "netd to come up");
            if !console.contains("netd: ready, at most ") {
                return Err(format!("netd never came up on a machine with a NIC:\n{console}"));
            }

            let result = qemu.run_test("test_rs_netd_hostile_peer", Duration::from_secs(120));
            if let Some(err) = &result.error {
                return Err(format!("{err}\n{}", result.stdout));
            }
            if result.exit_code != Some(0) {
                return Err(format!(
                    "netd_hostile_peer exited {:?}:\n{}",
                    result.exit_code, result.stdout
                ));
            }

            // The guest's own case list, restated here so a case deleted on
            // one side is a red run rather than a quieter test.
            const CASES: usize = 6;
            let Some(refused) = result
                .stdout
                .split("hostile peer: ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
            else {
                return Err(format!(
                    "netd_hostile_peer printed no count:\n{}",
                    result.stdout
                ));
            };
            if refused != CASES {
                return Err(format!(
                    "netd refused {refused} malformed frames, not {CASES}:\n{}",
                    result.stdout
                ));
            }

            // `TestResult::serial` is everything the console carried while the
            // guest ran, netd's own lines included — the daemon and the test
            // share one window (`issues/build/`), which here is what makes the
            // daemon's side of the story readable at all.
            console.push_str(&result.serial);
            let named = "netd: dropping client";
            if !console.contains(named) {
                return Err(format!(
                    "netd got rid of clients without a `{named}` line — a daemon that drops \
                     peers silently cannot be asked what happened:\n{console}"
                ));
            }
            serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
            eprintln!("  [netcase] {refused} hostile frames refused, netd named every peer it dropped");
            Ok(())
        }
        "launcher_refusals" => {
            // **`/system/bin/init` is the one process the machine cannot lose**, and
            // every launcher client — the compositor, every terminal, every
            // shell, sshd — can send it whatever it likes. The guest carries
            // the verdicts: init answered, init is still launching, and the
            // kernel's live-object count did not grow across sixteen refused
            // launches. The host carries the one the guest cannot see —
            // whether init said anything about what it refused.
            //
            // `tests/netcase` because its test-runner is the only one that
            // receives a `launcher` connector, and because two boot programs
            // is the smallest blast radius for a test whose whole subject is
            // making init misbehave.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
            let bins: Vec<(String, Vec<u8>)> = rust_bins
                .iter()
                .filter(|(name, _)| name == "launcher_refusals")
                .cloned()
                .collect();
            if bins.is_empty() {
                return Err("launcher_refusals was not built".to_string());
            }
            let mut qemu = QemuInstance::boot_with_options(
                &config,
                &[],
                &bins,
                BootOptions {
                    profile: qemu::Profile::Headless,
                    // The live-object count is a `SYS_DEBUG` action, and a
                    // shipping kernel has none: both readings would be the same
                    // `InvalidArgument` and the leak arm would pass having
                    // counted nothing.
                    kernel_features: ACTUATOR_KERNEL,
                    ..Default::default()
                },
            );
            let mut console = qemu.boot_log().to_string();
            let _ = await_marker(&mut qemu, &mut console, "===READY===", "test-runner to come up");

            let result = qemu.run_test("test_rs_launcher_refusals", Duration::from_secs(120));
            if let Some(err) = &result.error {
                return Err(format!("{err}\n{}", result.stdout));
            }
            if result.exit_code != Some(0) {
                return Err(format!(
                    "launcher_refusals exited {:?}:\n{}",
                    result.exit_code, result.stdout
                ));
            }
            console.push_str(&result.serial);
            if !console.contains("init: launcher: cannot start") {
                return Err(format!(
                    "init refused a launch without a line saying so — a launcher that \
                     drops requests silently cannot be asked what happened:\n{console}"
                ));
            }
            serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
            eprintln!("  [netcase] init refused three bad launches, named them, and kept launching");
            Ok(())
        }
        "spawn_cwd" => {
            // A child starts in the directory its spawn names — through the
            // launcher from the shell's `cd`, through the launcher from
            // `Command::current_dir`, and directly — and a spawn into a
            // directory that is not there is refused by name. The guest carries
            // every verdict; `tests/netcase` is where a launcher and a declared
            // shell exist to tell the roads apart.
            let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase");
            let bins: Vec<(String, Vec<u8>)> =
                rust_bins.iter().filter(|(name, _)| name == "spawn_cwd").cloned().collect();
            if bins.is_empty() {
                return Err("spawn_cwd was not built".to_string());
            }
            let mut qemu = QemuInstance::boot_with_options(
                &config,
                &[],
                &bins,
                BootOptions { profile: qemu::Profile::Headless, ..Default::default() },
            );
            let mut console = qemu.boot_log().to_string();
            let _ = await_marker(&mut qemu, &mut console, "===READY===", "test-runner to come up");

            let result = qemu.run_test("test_rs_spawn_cwd", Duration::from_secs(120));
            if let Some(err) = &result.error {
                return Err(format!("{err}\n{}", result.stdout));
            }
            if result.exit_code != Some(0) {
                return Err(format!(
                    "spawn_cwd exited {:?}:\n{}{}",
                    result.exit_code, result.stdout, result.serial
                ));
            }
            if !result.stdout.contains("spawn-cwd: every child started where its spawn said") {
                return Err(format!("spawn_cwd exited 0 without its verdict:\n{}", result.stdout));
            }
            console.push_str(&result.serial);
            serial::Serial::named("boot console", console.as_str()).must_be_clean()?;
            eprintln!("  [netcase] every child started in the directory its spawn named");
            Ok(())
        }
        "input_claim_absent" => {
            // The one bootable machine with no input source: no xHCI, the
            // i8042 taken away. Three channels must agree — the argv stages
            // the absence, the kernel's drivers report it, and the claim
            // syscall refuses by name.
            let options = BootOptions {
                profile: qemu::Profile::MetalNoUsb,
                i8042: false,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            if !argv.iter().any(|a| a.contains("i8042=off")) {
                return Err("the i8042 is on; a PS/2 keyboard could feed the stream".to_string());
            }
            for banned in ["nec-usb-xhci", "usb-kbd", "usb-mouse", "usb-tablet", "usb-storage"] {
                if let Some(a) = argv.iter().find(|a| a.contains(banned)) {
                    return Err(format!("{a:?} on the machine whose point is having no USB"));
                }
            }
            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();
            for want in
                ["i8042: absent", "xHCI: no controller on this machine, USB input unavailable"]
            {
                if !boot.contains(want) {
                    return Err(format!("the kernel never said {want:?}:
{boot}"));
                }
            }
            let result = qemu.run_test("test_rs_input_absent", Duration::from_secs(30));
            if let Some(err) = &result.error {
                return Err(format!("{err}
{}", result.stdout));
            }
            if result.exit_code != Some(0) {
                return Err(format!(
                    "input_absent exited {:?}:
{}",
                    result.exit_code, result.stdout
                ));
            }
            for want in ["keyboard: refused NotFound", "mouse: refused NotFound"] {
                if !result.stdout.contains(want) {
                    return Err(format!("missing {want:?}:
{}", result.stdout));
                }
            }
            eprintln!("  [input] no input source exists and both claims refused NotFound");
            Ok(())
        }
        "gpu_set_resolution" => {
            /// Mirrored in `tests/toyos-rust-tests/src/bin/gpu_set_resolution.rs`.
            const WANT: (usize, usize) = (800, 600);

            let options = BootOptions {
                profile: qemu::Profile::VirtioGpu,
                qmp: true,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            // Prefix, not equality: a virtio function on a machine with a unit
            // carries `iommu_platform=on` behind its name.
            if !argv.windows(2).any(|w| w[0] == "-device" && w[1].starts_with("virtio-gpu-pci")) {
                return Err(format!("the profile stages no virtio-gpu: {argv:?}"));
            }
            // A `-vga` adapter beside it is a second display, and firmware
            // would publish a GOP the kernel could take instead.
            if argv.windows(2).any(|w| w[0] == "-vga" && w[1] != "none") {
                return Err(format!("a second display is on the machine: {argv:?}"));
            }

            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
            let boot = qemu.boot_log().to_string();
            for want in ["VirtIO GPU: display ", "GPU: using VirtIO"] {
                if !boot.contains(want) {
                    return Err(format!("the kernel never said {want:?}:\n{boot}"));
                }
            }

            // **The verdict is QEMU's own scanout, not the guest's account of
            // itself**: the dump's header is the size SET_SCANOUT named.
            let before = qemu.screendump();
            let result = qemu.run_test("test_rs_gpu_set_resolution", Duration::from_secs(30));
            if let Some(err) = &result.error {
                return Err(format!("the guest stopped answering: {err}\n{}", result.stdout));
            }
            if !check_rust_result(&result) {
                return Err(format!("gpu_set_resolution failed:\n{}", result.stdout));
            }
            if !result.stdout.contains("===GPU_RESOLUTION_OK===") {
                return Err(format!("the guest never reached its marker:\n{}", result.stdout));
            }
            let after = qemu.screendump();

            if (before.width, before.height) == (after.width, after.height) {
                return Err(format!(
                    "QEMU's scanout is {}x{} before and after, so nothing the guest did \
                     reached the device",
                    after.width, after.height
                ));
            }
            if (after.width, after.height) != WANT {
                return Err(format!(
                    "the guest asked for {}x{} and QEMU's own scanout is {}x{}:\n{}",
                    WANT.0, WANT.1, after.width, after.height, result.stdout
                ));
            }
            eprintln!(
                "  [gpu] QEMU's scanout went {}x{} to {}x{}, which is the mode the guest asked \
                 for and the mode a second claim was told",
                before.width, before.height, after.width, after.height
            );
            Ok(())
        }
        "metal_sim_input" => {
            // M2's exit criterion, on the machine shape and the kernel that
            // get flashed: no virtio device, no USB HID — so the i8042 is the
            // guest's only input device — and no kernel feature turned on for
            // the occasion, unlike the four tests above it.
            //
            // What it asserts is the events, read by an in-guest process and
            // printed. The first version asserted screen pixels after a click
            // at a fixed taskbar coordinate, which made the compositor's
            // layout part of a kernel-delivery criterion and needed thresholds
            // to survive the taskbar's own once-a-second repaint. M2 owns
            // delivery — pin to userland process — so that is what this
            // measures, and nothing here says the compositor reacted.
            // `metal_sim_compositor` is what covers the compositor.
            let options = BootOptions {
                profile: qemu::Profile::Metal,
                qmp: true,
                ..Default::default()
            };
            let argv = qemu::profile_argv(&options);
            metal_sim_argv_check(&argv)?;
            if argv.iter().any(|a| a.contains("i8042=off")) {
                return Err("metal-sim turned the i8042 off".to_string());
            }

            let mut qemu =
                QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);

            // `kernel/src/mouse.rs` scales each relative count into the
            // 0..32767 space the compositor consumes, per axis and derived
            // from the screen — so the kernel is asked what it used rather
            // than the constant being copied here, which would stop being a
            // check the moment either side changed.
            let boot = qemu.boot_log().to_string();
            let Some((scale_x, scale_y)) = parse_rel_scale(&boot) else {
                return Err(format!("the kernel never said what pointer scale it used:\n{boot}"));
            };
            const DX: i32 = 40;
            const DY: i32 = -30;
            // Off the origin first — the accumulated position clamps at 0, so a
            // move up or left from there is invisible. Under 256 counts, or the
            // packet's overflow bit is set and the motion is dropped by design.
            let (result, sent) = input_events_run(&mut qemu, (200, 200), (DX, DY));
            if let Some(err) = &result.error {
                return Err(format!("{err} after {sent} of the sequence\n{}", result.stdout));
            }

            let keys = parse_key_events(&result.stdout);
            let typed: String = keys
                .iter()
                .filter(|e| e.modifiers & 0x10 == 0)
                .map(|e| e.translated.as_str())
                .collect();
            if !typed.contains("hello") {
                return Err(format!(
                    "typed {typed:?}, want it to contain \"hello\" — the keyboard never reached userland:\n{}",
                    result.stdout
                ));
            }

            let pointer = parse_mouse_events(&result.stdout);
            // The delta the wire carried, not "it moved": a sign error in dy
            // and a dropped high bit both survive "it moved", and the PS/2
            // wire points the opposite way to the screen. Relative, so it
            // says nothing about where any compositor would draw a cursor.
            let want = (DX * scale_x, DY * scale_y);
            let deltas: Vec<(i32, i32)> = pointer
                .windows(2)
                .map(|w| (w[1].x as i32 - w[0].x as i32, w[1].y as i32 - w[0].y as i32))
                .collect();
            if !deltas.contains(&want) {
                return Err(format!(
                    "no pointer event moved by {want:?}; deltas seen: {deltas:?}\n{}",
                    result.stdout
                ));
            }
            let Some(down) = pointer.iter().position(|e| e.buttons == 0x01) else {
                return Err(format!(
                    "no left-button-down event; buttons seen: {:?}",
                    pointer.iter().map(|e| e.buttons).collect::<std::collections::BTreeSet<_>>()
                ));
            };
            if !pointer[down + 1..].iter().any(|e| e.buttons == 0x00) {
                return Err(format!(
                    "the left button went down and never came up: {pointer:?}"
                ));
            }
            eprintln!(
                "  [metal-sim] {} key events (typed {typed:?}), {} pointer events, delta {want:?} delivered",
                keys.len(),
                pointer.len()
            );
            Ok(())
        }
        other => Err(format!("unknown input test {other}")),
    }
}

#[derive(Debug)]
struct KeyLine {
    usage: u8,
    modifiers: u8,
    translated: String,
}

/// `kev usage=0x04 mods=0x00 tr="a"` — what the in-guest reader prints.
fn parse_key_events(stdout: &str) -> Vec<KeyLine> {
    stdout
        .lines()
        .filter_map(|line| {
            let rest = line.split("kev usage=0x").nth(1)?;
            let (usage, rest) = rest.split_once(" mods=0x")?;
            let (modifiers, rest) = rest.split_once(" tr=")?;
            let translated = rest.trim().trim_matches('"');
            Some(KeyLine {
                usage: u8::from_str_radix(usage, 16).ok()?,
                modifiers: u8::from_str_radix(modifiers, 16).ok()?,
                translated: unescape(translated),
            })
        })
        .collect()
}

/// The guest prints through `{:?}`, so an escape sequence arrives as the
/// four characters `\u{1b}` rather than the byte.
fn unescape(s: &str) -> String {
    s.replace("\\u{1b}", "\u{1b}").replace("\\\"", "\"").replace("\\\\", "\\")
}

#[derive(Debug)]
struct MouseLine {
    buttons: u8,
    x: u16,
    y: u16,
}

/// `mev buttons=0x01 x=6400 y=6400` — what the in-guest reader prints.
fn parse_mouse_events(stdout: &str) -> Vec<MouseLine> {
    stdout
        .lines()
        .filter_map(|line| {
            let rest = line.split("mev buttons=0x").nth(1)?;
            let (buttons, rest) = rest.split_once(" x=")?;
            let (x, y) = rest.split_once(" y=")?;
            Some(MouseLine {
                buttons: u8::from_str_radix(buttons, 16).ok()?,
                x: x.parse().ok()?,
                y: y.trim().parse().ok()?,
            })
        })
        .collect()
}

/// The block count the NVMe driver derived, out of
/// `NVMe: block device id=1 blocks=62514774 (244198MB)`.
fn parse_nvme_blocks(log: &str) -> Option<u64> {
    log.lines()
        .find_map(|l| l.split("NVMe: block device id=").nth(1))?
        .split("blocks=")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The first number after `marker`, which both caches print their ceiling as
/// exactly once at boot.
fn parse_cache_budget(log: &str, marker: &str) -> Option<u64> {
    log.lines()
        .find_map(|l| l.split(marker).nth(1))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Every `<prefix>N evictions, R/M <unit>` line, as (evictions, resident).
///
/// The kernel emits one per full turnover of the cache, so the series is the
/// shape of the answer: a cache that evicts has a climbing first column and a
/// flat second, and a cache that only grows has no lines at all.
fn parse_cache_series(log: &str, prefix: &str, unit: &str) -> Vec<(u64, u64)> {
    log.lines()
        .filter_map(|l| {
            let tail = l.split(prefix).nth(1)?;
            if !tail.contains(unit) {
                return None;
            }
            let evictions = tail.split(" evictions,").next()?.trim().parse().ok()?;
            let resident = tail.split("evictions, ").nth(1)?.split('/').next()?.parse().ok()?;
            Some((evictions, resident))
        })
        .collect()
}

/// `file cache: E evictions, R/M pages resident, D dirty` as (E, R, D).
///
/// The dirty count is required, not optional: it is the only lawful reading
/// of a sample over budget, so a kernel line that stops carrying it drops
/// out of the series and fails the length assertion rather than passing as
/// a bound nobody checked.
fn parse_file_cache_series(log: &str) -> Vec<(u64, u64, u64)> {
    log.lines()
        .filter_map(|l| {
            let tail = l.split("file cache: ").nth(1)?;
            if !tail.contains("pages resident") {
                return None;
            }
            let evictions = tail.split(" evictions,").next()?.trim().parse().ok()?;
            let resident = tail.split("evictions, ").nth(1)?.split('/').next()?.parse().ok()?;
            let dirty = tail.split("resident, ").nth(1)?.split(" dirty").next()?.parse().ok()?;
            Some((evictions, resident, dirty))
        })
        .collect()
}

/// How many blocks the page cache's index has room for, out of
/// `page cache: … index sized for C cached blocks, cap S slots, B index bytes`.
fn parse_page_cache_index(log: &str) -> Option<u64> {
    log.lines()
        .find_map(|l| l.split("index sized for ").nth(1))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Decode one bcachefs superblock straight out of a disk image, with the
/// same parser the kernel uses — magic, version and CRC all checked.
fn read_superblock(image: &Path, block: u64) -> Result<bcachefs::Superblock, String> {
    common::storage::superblock_at(image, block)
}

/// A disk image's apparent size and the bytes it actually occupies. The gap
/// between the two is the whole reason a 244 GB test device is affordable.
fn image_extent(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
    (meta.len(), meta.blocks() * 512)
}


#[derive(Debug)]
struct XhciBind {
    kind: String,
    int_ring: usize,
}

/// `xHCI: USB keyboard ready on slot 2, int_ring +0xa000` — one line per HID
/// the driver bound, carrying the DMA offset of the ring that device's reports
/// arrive on. The offset is in the line because two devices sharing one ring
/// is invisible from outside: both keyboards still enumerate, still bind, and
/// still deliver — until the second one's TRBs land on top of the first's.
fn parse_xhci_binds(log: &str) -> Vec<XhciBind> {
    log.lines()
        .filter_map(|line| {
            let rest = line.split("xHCI: USB ").nth(1)?;
            let (kind, rest) = rest.split_once(" ready on slot ")?;
            let (_slot, rest) = rest.split_once(", int_ring +0x")?;
            Some(XhciBind {
                kind: kind.to_string(),
                int_ring: usize::from_str_radix(rest.split_whitespace().next()?, 16).ok()?,
            })
        })
        .collect()
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
        for probe in [
            "revoke-selftest: /tmp/revoke_probe",
            "revoke-selftest: /home/revoke_probe",
            "pc-unbind-selftest:",
        ] {
            let Some(verdict) = log.lines().find(|l| l.contains(probe)) else {
                return Err(format!("{probe} never ran:\n{log}"));
            };
            if !verdict.contains("PASS") {
                return Err(format!("{}\n{log}", verdict.trim()));
            }
            eprintln!("  [read-fault] {}", verdict.trim());
        }
        Ok(())
}

/// Two "acquire before a fallible step" controls: each count returned to its baseline after a refused call.
///
/// Text in, a verdict out: every line it reads is a kernel record, so the
/// T14's readback and a QEMU boot log are judged by this one predicate.
fn leak_rollback(log: &str) -> Result<(), String> {
        for probe in ["leak-selftest: device-mint", "leak-selftest: fat-reopen"] {
            let Some(verdict) = log.lines().find(|l| l.contains(probe)) else {
                return Err(format!("{probe} never ran:\n{log}"));
            };
            if !verdict.contains("PASS") {
                return Err(format!("{}\n{log}", verdict.trim()));
            }
            eprintln!("  [leak] {}", verdict.trim());
        }
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

/// Eight malformed extended-capability lists refused, and the handoff on the real controller.
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
        // And the walk on the controller QEMU does provide.
        let Some(real) = log
            .lines()
            .find(|l| l.contains("USB Legacy Support") || l.contains("ownership"))
        else {
            return Err(format!("no line about the handoff at all:\n{log}"));
        };
        // The handoff must precede the reset — a reset that already
        // happened is what the whole capability exists to avoid.
        let reset = log
            .find("xHCI: controller reset")
            .ok_or_else(|| format!("the controller was never reset:\n{log}"))?;
        let handoff = log.find(real).expect("just found");
        if handoff > reset {
            return Err(format!(
                "the ownership handoff runs after HCRST, which is no handoff at all:\n{log}"
            ));
        }
        // A controller that still enumerates its bus afterwards.
        if !log.contains("xHCI: controller started") {
            return Err(format!("the controller did not come up:\n{log}"));
        }
        eprintln!("  [xhci] {}", verdict.trim());
        eprintln!("  [xhci] {}", real.trim());
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

/// Whether `line` is the probe's last word: each of its three outcomes.
fn sysret_ss_reported(line: &str) -> bool {
    [SYSRET_SS_RELOADED, SYSRET_SS_NOT_RELOADED, SYSRET_SS_UNARMED]
        .iter()
        .any(|end| line.contains(end))
}

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
                "the SS-reload probe never reported — iod may not have run it:\n{log}"
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
        for site in ["boot", "iod"] {
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

/// The machine's kernel threads are hosted.
///
/// Text in, a verdict out: every line is a `log!` record, so the T14's
/// readback and a QEMU boot log are judged by this one predicate.
fn klogd_hosted(boot: &serial::Serial) -> Result<(), String> {
    boot.must_be_clean()?;
    for name in ["klogd", "iod"] {
        let line = boot.must_say(&format!("kthread: {name}"))?;
        eprintln!("  [kthread] {}", line.trim());
    }
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

/// `latency_wake` on a machine with no console: the p99, off the kernel's own
/// exit record.
///
/// A negative code is `cyclictest`'s refusal and not a fast machine — the sign
/// is the whole of what separates the two, and that contract is in the binary's
/// own module header.
fn wake_latency_recorded(boot: &metal::Readback) -> Result<(), String> {
    let code = boot.exit_code("test_rs_cyclictest")?;
    if code < 0 {
        return Err(format!(
            "cyclictest exited {code}, which is a refusal and not a measurement: -1 is no \
             capability endowed, -2 is the real-time band refused"
        ));
    }
    // cyclictest's `BUCKETS`: a p99 there is a floor and not a measurement.
    if code >= 4096 {
        return Err(format!("cyclictest's p99 is {code} us, past its histogram"));
    }
    boot.measured("latency.p99_us", u64::try_from(code).expect("a non-negative code"));
    Ok(())
}

/// The negative control, executed: an AP left holding what `INIT` gave it, and
/// [`control_regs`] refusing the machine that produces.
///
/// The one link the two tests above do not cover. [`control_regs`] reads a
/// healthy boot and [`control_regs_verdict`] reads values typed into this file;
/// between them sits the question of whether the verdict would recognise a real
/// divergent CPU, and a `no-ap-control-regs` kernel nothing runs answers it in
/// prose. The boot dies here — the kernel's own assertion kills it — but
/// `self_check` logs *before* it asserts, exactly so that the values a CPU
/// failed with survive the failure, and that is what this reads.
///
/// `smp=2` because the first AP to check itself panics and `halt_all_cpus`
/// follows it: any CPU after that one is a line that never arrives, and the
/// refusal would then be about the count rather than about the registers.
/// [`qemu::Profile::Metal`] because there the 16550 is the console: a guest that
/// dies during `boot_aps` has no virtio-console yet, and this way one channel
/// carries the per-CPU line and the panic that follows it.
fn control_regs_negative(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const CPUS: u32 = 2;
    // The AP's own line, which arrives whether or not the actuator did anything
    // — so a feature that silently stopped working is a named failure below
    // rather than a boot timeout with nothing to read.
    const MARKER: &str = "control_regs: cpu1 cr0=";

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            smp: CPUS,
            profile: qemu::Profile::Metal,
            kernel_params: &["no-ap-control-regs"],
            ready_marker: MARKER,
            ..Default::default()
        },
    );
    let mut log = qemu.boot_log().to_string();
    log += &qemu.drain_until(Duration::from_secs(20), |l| l.contains("the declaration is"));

    // The premise: a divergent CPU, not merely a dead boot. Anything can kill a
    // boot, and a test that only asserted the panic would pass on a kernel
    // whose registers were right and whose assertion was wrong.
    let Err(refusal) = control_regs(&log, CPUS) else {
        return Err(format!(
            "the verdict accepted a `no-ap-control-regs` boot — either the actuator did \
             nothing or the verdict cannot see the machine it was written for\n{log}"
        ));
    };
    // Named, and named for a bit rather than for the count: a refusal about a
    // missing line or an unreadable one satisfies `is_err` and means nothing.
    //
    // **`WP`, not `CD`, and that is a finding rather than a preference.** `CD`
    // is the consequence this whole file exists for, and it is the obvious bit
    // to demand — but a guest cannot hold it under KVM. Measured 2026-08-08:
    // an AP that has executed nothing but the trampoline reads `cr0=0xe0000011`
    // under this host's TCG and `cr0=0x80000011` on an Intel Xeon 6973P-C KVM
    // runner (CI run 31278396401, shard 3), `CD` and `NW` clear, everything
    // else identical. So `CD` here would be a gate that only one of the two
    // machines this suite runs on can fail. `WP` is absent on the AP either
    // way, and its consequence — the kernel's own read-only mappings not
    // binding supervisor writes — does not depend on the hypervisor.
    if !refusal.contains("cpu1") || !refusal.contains("WP") {
        return Err(format!(
            "the verdict refused for something other than cpu1's write protection: {refusal}"
        ));
    }
    // Where the host does leave `CD` set, it is demanded, so the arm that *can*
    // see the caching defect does not quietly become the weaker of the two.
    if !common::qemu::SUITE_ARCH.accel().is_hardware() && !refusal.contains("CD") {
        return Err(format!(
            "TCG leaves an AP's `CD` set and the refusal does not name it: {refusal}"
        ));
    }
    // And the kernel refused too, on its own assertion rather than on a fault
    // somewhere downstream of one — the shipped check, on the shipped line. The
    // declaration is a constant, so it is the same number on either host.
    for want in ["control_regs: cpu1 holds cr0=", "the declaration is 0x80010033"] {
        if !log.contains(want) {
            return Err(format!("the kernel never said {want:?}:\n{log}"));
        }
    }
    eprintln!("  [control_regs] a real divergent AP, refused: {refusal}");
    Ok(())
}

/// A non-last AP that never starts must leave no dead slot in `0..cpu_count()`.
///
/// `smp-skip-ap` skips the startup of the AP that would be cpu2 on this four-vCPU
/// machine. The unfixed kernel spent cpu2's id before it ran and counted a later
/// AP anyway, so a shootdown after `set_ready` waited on a slot no CPU carried and
/// the machine died. The verdict is survival plus density: `smp_hole_shootdown`
/// frees pages back eight times and its marker prints, cpu1 comes online, and
/// neither cpu2 nor the cpu3 behind it joins.
fn smp_failed_ap_leaves_no_hole(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions {
        smp: 4,
        kernel_params: &["smp-skip-ap"],
        ..Default::default()
    };
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    let boot = qemu.boot_log().to_string();

    // The premise, not just a dead boot: cpu1 came up and cpu2 failed.
    if !boot.contains("SMP: AP cpu1 lapic=") || !boot.contains(" online") {
        return Err(format!(
            "cpu1 never came online, so the boot did not stage a non-last failed AP:\n{boot}"
        ));
    }
    if !boot.contains("SMP: AP cpu2 lapic=") || !boot.contains("failed to start!") {
        return Err(format!("the actuator did not fail cpu2's bring-up:\n{boot}"));
    }

    let result = qemu.run_test("test_rs_smp_hole_shootdown", Duration::from_secs(30));
    if let Some(err) = &result.error {
        // The unfixed kernel's signature: the guest stops answering.
        return Err(format!(
            "the guest stopped answering — a shootdown after a failed AP took the machine \
             down:\n{err}\nserial:\n{}",
            result.serial
        ));
    }
    if !check_rust_result(&result) {
        return Err(format!("smp_hole_shootdown failed:\n{}", result.stdout));
    }

    // Density: a "joining" line for cpu2 or cpu3 would be a slot past the failed AP.
    let serial = format!("{boot}\n{}", result.serial);
    for phantom in ["CPU 2: joining scheduler", "CPU 3: joining scheduler"] {
        if serial.contains(phantom) {
            return Err(format!(
                "a CPU past the failed AP joined, so `0..cpu_count()` is not the online \
                 set: {phantom:?}\n{serial}"
            ));
        }
    }
    eprintln!("  [smp] a non-last AP failed and the dense machine survived its shootdowns");
    Ok(())
}

/// Everything the driver derived its DMA pool from, off the two lines it
/// prints. Reading these is what makes a test see a *derivation* rather than
/// the fact that some number was printed: every fixed cap that ever stood
/// where `Layout::new`'s `.min(max_slots)` stands now leaves six devices
/// enumerating on six rings and is invisible from every other angle.
#[derive(Debug)]
struct XhciLayout {
    /// `xHCI: max_slots=64 max_ports=12 …`, straight off HCSPARAMS1.
    cap_slots: usize,
    pool_kib: usize,
    scratchpad: usize,
    blocks: usize,
    stride: usize,
}

/// The kernel's account of the machine's aperture, held against addresses this
/// harness read off the same boot without it: the `bars=` of the ECAM walk, and
/// the windows `publish` said it cut.
///
/// Asking firmware where its root bridges decode, decoding what it answers and
/// carrying the answer across `KernelArgs` all fail into the one missing record.
fn aperture_account(log: &serial::Serial) -> Result<(), String> {
    fn hex(s: &str) -> Result<u64, String> {
        u64::from_str_radix(s.trim().trim_start_matches("0x"), 16)
            .map_err(|e| format!("{s:?} is not an address: {e}"))
    }

    let named = log.must_say("pcidev: firmware declared root bridge memory: ")?;
    let windows = named
        .rsplit_once("memory: ")
        .ok_or_else(|| format!("unparseable aperture record: {named:?}"))?
        .1
        .split(", ")
        .map(|w| {
            let (base, end) = w
                .trim()
                .strip_prefix("mem ")
                .and_then(|r| r.split_once(".."))
                .ok_or_else(|| format!("unparseable window {w:?} on {named:?}"))?;
            Ok((hex(base)?, hex(end)?))
        })
        .collect::<Result<Vec<(u64, u64)>, String>>()?;

    // Every memory BAR firmware itself assigned, off the enumeration rather
    // than off the record being judged. A BAR outside every named window is a
    // fact about the machine and the kernel has to name it — one line each,
    // and none for a machine where there are none.
    let outside: Vec<String> = log
        .text()
        .lines()
        .filter_map(|l| Some((l, l.split("bars=[").nth(1)?.split_once(']')?.0)))
        .flat_map(|(l, bars)| bars.split_whitespace().map(move |b| (l, b)))
        .filter_map(|(l, bar)| Some((l, hex(bar.split_once('=')?.1).ok()?)))
        .filter(|(_, at)| !windows.iter().any(|(base, end)| at >= base && at < end))
        .map(|(l, at)| format!("{at:#x} on {}", l.trim()))
        .collect();
    let said: Vec<&str> = log
        .text()
        .lines()
        .filter(|l| l.contains("is inside none of it, so this kernel has no declaration"))
        .collect();
    if said.len() != outside.len() {
        return Err(format!(
            "the enumeration puts {} assigned memory BAR(s) outside {named:?} and the kernel \
             named {}:\nthe harness: {outside:#?}\nthe kernel: {said:#?}",
            outside.len(),
            said.len(),
        ));
    }

    // And every address this kernel put a BAR at carries its own standing
    // against those windows, recomputed here from the record's address rather
    // than read off the clause beside it. A placement accounted from an empty
    // window list says "inside no window firmware named" about an address that
    // is inside one, and that is what this refuses.
    let mut placements = 0usize;
    for line in log.text().lines().filter(|l| l.contains(" placed at 0x")) {
        let at = hex(
            line.split_once(" placed at 0x")
                .and_then(|(_, r)| r.split(' ').next())
                .ok_or_else(|| format!("unparseable placement record: {line:?}"))?,
        )?;
        let account = match windows.iter().find(|(base, end)| at >= *base && at < *end) {
            Some((base, _)) => format!("inside firmware's mem {base:#x}"),
            None => "inside no window firmware declared".to_string(),
        };
        if !line.contains(&account) {
            return Err(format!(
                "{at:#x} is {account} by {named:?}, and the kernel's own record of putting a BAR \
                 there says otherwise:\n{}",
                line.trim()
            ));
        }
        placements += 1;
    }
    if placements == 0 {
        return Err(format!(
            "this boot put no BAR anywhere, so nothing on it accounts for an address against \
             {named:?}:\n{}",
            log.text()
        ));
    }
    Ok(())
}

/// One PCI function as both readers name it: bus, device, function, vendor,
/// device id.
type PciFunction = (u8, u8, u8, u16, u16);

fn describe_pci_function(f: &PciFunction) -> String {
    let (bus, dev, func, vendor, device) = f;
    format!("{bus:02x}:{dev:02x}.{func} {vendor:04x}:{device:04x}")
}

/// The guest's account: every `PCI bb:dd.f [cc..] vendor=vvvv device=dddd`
/// line the kernel's ECAM walk printed.
fn guest_pci_functions(log: &str) -> Result<BTreeSet<PciFunction>, String> {
    let mut out = BTreeSet::new();
    for line in log.lines() {
        let Some(rest) = line.split("  PCI ").nth(1) else { continue };
        let Some((bdf, rest)) = rest.split_once(" [") else { continue };
        let parse = || -> Option<PciFunction> {
            let (bus, df) = bdf.split_once(':')?;
            let (dev, func) = df.split_once('.')?;
            let vendor = rest.split("vendor=").nth(1)?.split_whitespace().next()?;
            let device = rest.split("device=").nth(1)?.split_whitespace().next()?;
            Some((
                u8::from_str_radix(bus, 16).ok()?,
                u8::from_str_radix(dev, 16).ok()?,
                func.parse().ok()?,
                u16::from_str_radix(vendor, 16).ok()?,
                u16::from_str_radix(device, 16).ok()?,
            ))
        };
        let f = parse().ok_or_else(|| format!("unparseable guest PCI line: {line:?}"))?;
        if !out.insert(f) {
            return Err(format!("the guest printed one function twice: {line:?}"));
        }
    }
    Ok(out)
}

/// QEMU's account: `info pci`, whose entries are a `Bus N, device N,
/// function N:` header followed by a `PCI device vvvv:dddd` id line.
fn qmp_pci_functions(answer: &str) -> Result<BTreeSet<PciFunction>, String> {
    let mut out = BTreeSet::new();
    let mut at: Option<(u8, u8, u8)> = None;
    for line in answer.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("Bus ") {
            let parse = || -> Option<(u8, u8, u8)> {
                let (bus, rest) = rest.split_once(", device ")?;
                let (dev, rest) = rest.split_once(", function ")?;
                let func = rest.split_once(':')?.0;
                Some((bus.trim().parse().ok()?, dev.trim().parse().ok()?, func.trim().parse().ok()?))
            };
            at = Some(parse().ok_or_else(|| format!("unparseable info pci header: {line:?}"))?);
        } else if let Some(ids) = l.split("PCI device ").nth(1) {
            let (bus, dev, func) =
                at.ok_or_else(|| format!("an id line before any Bus header: {line:?}"))?;
            let parse = || -> Option<(u16, u16)> {
                let mut it = ids.split_whitespace().next()?.split(':');
                let vendor = u16::from_str_radix(it.next()?, 16).ok()?;
                let device = u16::from_str_radix(it.next()?, 16).ok()?;
                Some((vendor, device))
            };
            let (vendor, device) =
                parse().ok_or_else(|| format!("unparseable info pci id line: {line:?}"))?;
            if !out.insert((bus, dev, func, vendor, device)) {
                return Err(format!("info pci listed one function twice: {line:?}"));
            }
            at = None;
        }
    }
    if out.is_empty() {
        return Err(format!("info pci answered no functions at all:\n{answer}"));
    }
    Ok(out)
}

fn parse_xhci_layout(log: &str) -> Option<XhciLayout> {
    let cap = log.lines().find_map(|l| l.split("xHCI: max_slots=").nth(1))?;
    let dma = log.lines().find_map(|l| l.split("xHCI: dma ").nth(1))?;
    let (pool_kib, rest) = dma.split_once(" KiB: scratchpad=")?;
    let (scratchpad, rest) = rest.split_once(" device blocks=")?;
    let (blocks, rest) = rest.split_once(" of ")?;
    let stride = rest.split_once(" B (max_slots=")?.0;
    Some(XhciLayout {
        cap_slots: cap.split_whitespace().next()?.parse().ok()?,
        pool_kib: pool_kib.parse().ok()?,
        scratchpad: scratchpad.parse().ok()?,
        blocks: blocks.parse().ok()?,
        stride: stride.parse().ok()?,
    })
}

/// Every slot id in an `xHCI: slot 3 enabled ...` line, in order.
fn parse_xhci_slots(log: &str) -> Vec<u32> {
    log.lines()
        .filter_map(|line| line.split("xHCI: slot ").nth(1)?.split_once(" enabled"))
        .filter_map(|(slot, _)| slot.parse().ok())
        .collect()
}

/// One step of the `input_events` sequence, and how many lines the guest owes
/// for it.
enum Poke {
    Move(i32, i32),
    Button(&'static str, bool),
    Tap(&'static str),
}

/// What tells the `input_events` client its host has finished.
///
/// The right button, which no sequence driving that client produces for any
/// other reason, and the release rather than the press so the pointer is left
/// with nothing held. Every caller owes it one: without it the client waits out
/// its liveness ceiling.
pub(crate) fn input_events_end(input: &mut qemu::QmpInput) {
    input.mouse(0, 0, Some(("right", true)));
    input.mouse(0, 0, Some(("right", false)));
}

/// The `input_events` sequence: land off the origin, move by a named delta,
/// click, type `hello`, and finish on the right button the client exits on.
///
/// Every step waits for the guest to print what the step before it produced, so
/// the host never has more than one packet in flight and a device queue cannot
/// swallow one. `xhci_second_controller` measured the alternative at width 4:
/// four pointer events arrived and all five keys were lost, which reads exactly
/// like the defect it exists to catch.
fn input_events_run(
    qemu: &mut QemuInstance,
    home: (i32, i32),
    delta: (i32, i32),
) -> (TestResult, usize) {
    let script = [
        Poke::Move(home.0, home.1),
        Poke::Move(delta.0, delta.1),
        Poke::Button("left", true),
        Poke::Button("left", false),
        Poke::Tap("h"),
        Poke::Tap("e"),
        Poke::Tap("l"),
        Poke::Tap("l"),
        Poke::Tap("o"),
        // `input_events_end`, spelled out because the script paces every step
        // against an arrival and cannot hand two of them to someone else.
        Poke::Button("right", true),
        Poke::Button("right", false),
    ];
    let sent = std::cell::Cell::new(0usize);
    let result = {
        let mut input: Option<qemu::QmpInput> = None;
        let (mut mev, mut kev) = (0usize, 0usize);
        let (mut want_mev, mut want_kev) = (0usize, 0usize);
        qemu.run_test_paced("test_rs_input_events", Duration::from_secs(60), |socket, line| {
            if line.contains("===INPUT_READY===") {
                input = Some(qemu::QmpInput::open(
                    socket.expect("input_events needs BootOptions { qmp: true }"),
                ));
            }
            mev += usize::from(line.contains("mev buttons="));
            kev += usize::from(line.contains("kev usage="));
            let Some(input) = input.as_mut() else { return };
            if mev < want_mev || kev < want_kev {
                return;
            }
            let Some(poke) = script.get(sent.get()) else { return };
            match poke {
                Poke::Move(dx, dy) => {
                    input.mouse(*dx, *dy, None);
                    want_mev += 1;
                }
                Poke::Button(name, down) => {
                    input.mouse(0, 0, Some((name, *down)));
                    want_mev += 1;
                }
                Poke::Tap(key) => {
                    input.keys(&[(key, true), (key, false)]);
                    want_kev += 2;
                }
            }
            sent.set(sent.get() + 1);
        })
    };
    (result, sent.get())
}

/// The per-axis relative-pointer scale out of `mouse: rel scale x=64 y=64`.
///
/// Read from the kernel rather than restated here: `kernel/src/mouse.rs`
/// derives it from the screen, so a copy of the constant would stop being a
/// check the moment either side changed.
fn parse_rel_scale(log: &str) -> Option<(i32, i32)> {
    let (x, rest) = log
        .lines()
        .find_map(|l| l.split("mouse: rel scale x=").nth(1))?
        .split_once(" y=")?;
    Some((x.parse().ok()?, rest.split_whitespace().next()?.parse().ok()?))
}

/// The `-device` arguments naming an xHCI controller. A machine's controller
/// count is a shape claim, and argv is the only place it is visible: two
/// controllers where one carries nothing look identical from inside a guest
/// that never enumerated the second.
fn xhci_argv(argv: &[String]) -> Vec<&str> {
    argv.windows(2)
        .filter(|w| w[0] == "-device")
        .map(|w| w[1].as_str())
        .filter(|v| v.contains("usb-xhci"))
        .collect()
}

/// `(slot, source)` out of every `xHCI: pointer on slot 3 merges as source 2`.
///
/// The slot is there so the test can show the collision it is guarding
/// against: two pointers on one slot id of two different controllers is
/// exactly what a slot-derived button-merge source folded into one entry.
fn parse_pointer_sources(log: &str) -> Vec<(u32, u32)> {
    log.lines()
        .filter_map(|line| {
            let rest = line.split("xHCI: pointer on slot ").nth(1)?;
            let (slot, source) = rest.split_once(" merges as source ")?;
            Some((
                slot.parse().ok()?,
                source.split_whitespace().next()?.parse().ok()?,
            ))
        })
        .collect()
}

/// The `-device usb-*` arguments a profile passes, boot stick included.
fn usb_argv(argv: &[String]) -> Vec<&str> {
    argv.windows(2)
        .filter(|w| w[0] == "-device")
        .map(|w| w[1].as_str())
        .filter(|v| v.starts_with("usb-"))
        .collect()
}

/// The `keys=` field of an `i8042: drain ...` trace line.
fn trace_keys(line: &str) -> Option<usize> {
    line.split("i8042: drain ")
        .nth(1)?
        .split("keys=")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn build_test_registry(
    rust_bins: &[(String, Vec<u8>)],
    c_names: &[String],
) -> Vec<TestDef> {
    let mut tests = Vec::new();

    for name in discover_rust_tests(rust_bins) {
        let timeout = match name.as_str() {
            // Writes the child's whole image through bcachefs before it can run
            // it, which is the only thing here that is not a spawn.
            "disk_backtrace" => Duration::from_secs(15),
            _ => Duration::from_secs(5),
        };
        tests.push(TestDef {
            qemu_name: format!("test_rs_{name}"),
            check: check_for(&name),
            settle: settle_for(&name),
            timeout,
            name,
        });
    }

    for name in c_names {
        tests.push(TestDef {
            qemu_name: format!("test_c_{name}"),
            timeout: Duration::from_secs(10),
            check: check_c_result,
            settle: no_settle,
            name: name.clone(),
        });
    }

    tests
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

/// What one worker takes off the queue.
///
/// A boot, or the run of adjacent boots that share one guest — never a bare
/// test name, because [`group_boot`] makes adjacency in [`MACHINE_TESTS`]
/// load-bearing and a group split across two workers would boot two machines
/// and drain one console between them.
#[derive(Clone)]
enum Task<'a> {
    /// Rust and C tests on one lane's guests in turn ([`shared_boots`]), and
    /// the kernel they boot.
    ///
    /// Two blocks rather than one: [`ACTUATOR_TESTS`] needs `SYS_DEBUG` and
    /// everything else must not have it, which is what makes the second list
    /// the shipping binary's own coverage.
    Shared(Vec<&'a TestDef>, &'static [&'static str]),
    Machine(Vec<&'static str>),
    Screen(&'static str),
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

/// Two controllers, both with a codec that answers.
///
/// The kernel binds neither and names both. A first-match bind would go green
/// on every test that has one controller, so this is the arm that makes the
/// rule tested rather than merely written.
fn hda_two_live_refused(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    /// The kernel's word for soundd ending, and soundd's for taking the
    /// controller, which end the wait with this test's own sentence rather than
    /// the ceiling.
    const SOUNDD_GONE: &str = "exit: soundd";
    const HDA_PATH: &str = "soundd: hda path configured in";
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::HdaTwoLive, ..Default::default() },
    );
    // The refusal is a kernel boot line and is in the capture already; soundd's
    // answer to it is a userland line that races the ready marker.
    let mut text = qemu.boot_log().to_string();
    let stalled = await_guest(&mut qemu, &mut text, "soundd to say which sink it took", |seen| {
        [common::audio::NULL_SINK, SOUNDD_GONE, HDA_PATH].iter().any(|said| seen.contains(said))
    })
    .err();
    let log = serial::Serial::named("boot console", text.as_str());
    log.must_say("hda: 00:")?;
    log.must_say("has a live link (statests=")?;
    log.must_say("controllers answer on this machine")?;
    log.must_say("refused by name, no HDA audio")?;
    log.must_not_say("bound, statests=")?;
    // The machine still boots and still has a sink: absence of hardware is a
    // routing state, and a refusal must not be a machine that will not run.
    // **And it is the bind's absence and not a second spelling of the line
    // above**: init claims each class before it spawns, and soundd reaches the
    // null sink only where the endowment is missing — so this line requires
    // `device::try_claim(HdaAudio)` to have answered `Absent`.
    log.must_say(common::audio::NULL_SINK).map_err(|why| match stalled {
        Some(stall) => format!("{stall}\n{why}"),
        None => why,
    })?;
    log.must_say("Boot: complete")?;
    log.must_be_clean()
}

/// **A kernel that has declared itself corrupt runs nothing else.**
/// `halt_all_cpus` stops the other CPUs before anything else it does; a fatal
/// path that waited first — for a log, a drain, anything — would leave every
/// other CPU running userland under it. `test_rs_panic_halts_first` keeps three
/// siblings making kernel records while its main thread goes fatal.
///
/// **QEMU is the judge, and no clock is in it.** Once the fatal path has said
/// its line past the stop, `panic_reboot::arm`'s, every vCPU but the one that
/// went fatal must show [`qemu::stopped_cpus`]' `cli; hlt`. The fatal one holds
/// its panel under the shipped minute, so the machine is still there to ask.
fn panic_halts_the_others_first(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    const SMP: usize = 4;
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            smp: SMP as u32,
            kernel_features: ACTUATOR_KERNEL,
            qmp: true,
            ..Default::default()
        },
    );
    writeln!(qemu.stdin_mut(), "run test_rs_panic_halts_first").map_err(|e| format!("stdin: {e}"))?;
    qemu.flush_stdin();
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
        let stopped = qemu::stopped_cpus(&monitor.human("info registers -a"));
        if stopped.len() == SMP && stopped.iter().filter(|&&cpu| cpu).count() >= SMP - 1 {
            break;
        }
        if Instant::now() >= give_up {
            return Err(format!(
                "{STALLED} waiting for the other CPUs to halt after the fatal path on cpu{fatal} \
                 stopped them — QEMU shows each vCPU in `cli; hlt` as {stopped:?}\n{console}"
            ));
        }
        console.push_str(&qemu.drain_serial(Duration::from_millis(200)));
    }
    eprintln!("  [panic] the fatal path on cpu{fatal} left every other CPU in `cli; hlt`");
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
    /// What each tier held back, under the flag that runs it. Not a verdict and
    /// never red — it is the one thing a reader of the last line cannot infer
    /// from anything else in it, because a run that skipped a third of its cost
    /// looks exactly like a run that had nothing to do.
    relegated: Vec<(&'static str, Vec<String>)>,
}

impl Tally {
    fn new() -> Self {
        Tally {
            passed: 0,
            failures: Vec::new(),
            stalls: Vec::new(),
            invalid: Vec::new(),
            relegated: Vec::new(),
        }
    }

    /// The names this run's tier filter took out, for the summary to say so.
    fn holding_back(mut self, held: Vec<(Tier, Vec<String>)>) -> Self {
        self.relegated = held
            .into_iter()
            .filter(|(_, names)| !names.is_empty())
            .map(|(tier, names)| (tier.flag().expect("only a flag's tier is held back"), names))
            .collect();
        self
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

        // Above the result line and not below it, so the pointer is the last
        // thing before the verdict rather than an afterthought under it.
        for (flag, names) in &self.relegated {
            say(format!("not run without {flag}:"));
            say(format!("    {}", names.join(", ")));
            say(format!("    `cargo test --test toyos-build -- {flag}` runs them."));
            say(String::new());
        }

        // **In the result line, because that is the line a shard's job summary
        // extracts and the line anybody reads.** A count of what ran means
        // something different depending on how much was not attempted.
        let held: String = self
            .relegated
            .iter()
            .map(|(flag, names)| format!(", {} held back for {flag}", names.len()))
            .collect();
        match self.exit_code() {
            1 => say(format!(
                "test result: FAILED. {} passed, {} failed, {} invalidated, \
                 {total} total ({elapsed:.1?}){held}",
                self.passed,
                self.failures.len(),
                self.invalid.len(),
            )),
            2 => {
                say(format!(
                    "test result: INVALID. {} passed, {} invalidated by a \
                     host suspend of {suspended:.0?}, {total} total ({elapsed:.1?}){held}",
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
                "test result: ok. {} passed, {total} total ({elapsed:.1?}){held}",
                self.passed
            )),
        }
        out
    }
}

/// Which of the two shared boots a name belongs on — a *kernel build*, because
/// `SYS_DEBUG` is compiled in or it is not, and never a boot parameter.
fn shared_kernel(name: &str) -> &'static [&'static str] {
    if ACTUATOR_TESTS.contains(&name) {
        ACTUATOR_KERNEL
    } else {
        &[]
    }
}


/// The binaries and config every task boots with.
struct Bins<'a> {
    test_config: &'a Path,
    c_bins: &'a [(String, Vec<u8>)],
    rust_bins: &'a [(String, Vec<u8>)],
}

/// What a task's boots carry: its members' [`CARRIES`] rows, unioned.
fn carried_by(names: &[&str], bins: &Bins<'_>) -> qemu::Carried {
    let rows = CARRIES.iter().filter(|(test, _)| names.contains(test));
    qemu::carrying(bins.c_bins, bins.rust_bins, rows.flat_map(|(_, carries)| carries.iter().copied()))
}

/// The most test-binary bytes one shared boot carries. The list is run on
/// boots of one lane in turn, each carrying its own part, so what a shared
/// guest holds is bounded by this rather than by the list — and a guest's
/// memory is released between parts. A part costs one boot.
const SHARED_BOOT_BYTES: usize = 64 << 20;

/// `tests` cut in order into parts whose binaries fit [`SHARED_BOOT_BYTES`],
/// each with what it carries; a test whose own binaries do not fit is a part
/// by itself.
fn shared_boots<'a>(tests: &[&'a TestDef], bins: &Bins<'_>) -> Vec<(Vec<&'a TestDef>, qemu::Carried)> {
    let mut parts: Vec<Vec<&TestDef>> = Vec::new();
    let mut held: BTreeMap<String, usize> = BTreeMap::new();
    for &test in tests {
        let own = qemu::carrying(bins.c_bins, bins.rust_bins, [test.qemu_name.as_str()]).sizes();
        let mut with = held.clone();
        with.extend(own.clone());
        match parts.last_mut() {
            Some(part) if with.values().sum::<usize>() <= SHARED_BOOT_BYTES => {
                part.push(test);
                held = with;
            }
            _ => {
                parts.push(vec![test]);
                held = own;
            }
        }
    }
    parts
        .into_iter()
        .map(|part| {
            let carried =
                qemu::carrying(bins.c_bins, bins.rust_bins, part.iter().map(|t| t.qemu_name.as_str()));
            (part, carried)
        })
        .collect()
}

fn run_task(task: Task<'_>, bins: &Bins<'_>, report: &std::sync::mpsc::Sender<Outcome>) {
    // Both clocks, at every test, because what the host did *between* two of
    // them is a different question from what it did during one: a lid closed
    // while nothing was running invalidates nothing.
    let send = |name: String, reason: Option<String>, start: common::clock::Mark| {
        let _ = report.send(Outcome {
            name,
            reason,
            elapsed: start.elapsed(),
            suspended: start.suspended(),
        });
    };
    match task {
        Task::Shared(tests, features) => {
            let boots = shared_boots(&tests, bins);
            // The boot itself can fail, and it used to take the run with it.
            // Reporting the block's tests against its reason keeps the count
            // honest and says which one it died on.
            let mut done = 0usize;
            let outcome = catching(|| {
                // **The lane is the argument, and it is what makes the reboot
                // below unwritable in the order that broke it.** `boot` cannot
                // be called without a `LaneFree`, the only two things that
                // produce one are this line and `QemuInstance::shutdown`, and
                // `shutdown` takes the guest by value.
                let mut free = qemu::LaneFree::no_guest_yet();
                for (part, carried) in &boots {
                    eprintln!(
                        "  [shared] {} test(s) on {features:?}, carrying {} binaries, {} MiB",
                        part.len(),
                        carried.c.len() + carried.rust.len(),
                        carried.bytes() >> 20
                    );
                    let boot = |_: qemu::LaneFree| {
                        QemuInstance::boot_with_options(
                            bins.test_config,
                            &carried.c,
                            &carried.rust,
                            BootOptions { kernel_features: features, ..Default::default() },
                        )
                    };
                    let mut qemu = boot(free);
                    let mut reboots = 0usize;
                    for test in part {
                        let start = common::clock::mark();
                        let mut result = qemu.run_test(&test.qemu_name, test.timeout);
                        // **A guest that stopped answering is answered with a new
                        // one.** Its turn came, its whole ceiling passed, and it was
                        // never announced — so what this measured is the previous
                        // test's wreckage and not this one. Run `31241099454` is the
                        // bill: `abuse_gpu_resolution` took the shared boot with it
                        // and the 150 tests behind it each paid a full ceiling for a
                        // guest that was gone, 65 minutes of nothing and a job
                        // cancelled at 90.
                        //
                        // A reboot rather than an abandonment because every one of
                        // those tests still has a verdict owed to it, and the
                        // alternative is a suite that reports 150 reds it never ran.
                        // Bounded, because a block whose every member kills the
                        // guest must not boot one per test.
                        //
                        // **The old guest goes before the new one exists.** This
                        // was `qemu = boot()`, and Rust evaluates the right-hand
                        // side first: the replacement was launched, and waited on,
                        // while the instance it replaced still held the lane's
                        // `test-nvme-*.img` open for write. It exited 1 on QEMU's
                        // own image lock before saying anything, `wait_for_ready`
                        // panicked, and that panic escaped this block — so **every
                        // test still owed a verdict was reported red on it**. 129
                        // of one run's 131 reds carried that one sentence on
                        // 2026-08-17, against two real failures. The ordering is
                        // now the type's: `shutdown` takes the guest by value and
                        // is the only thing `boot` can be called with.
                        if result.boot_stopped_answering() && reboots < MAX_SHARED_REBOOTS {
                            reboots += 1;
                            eprintln!(
                                "  ---- the shared boot stopped answering before {}; rebooting \
                                 ({reboots}/{MAX_SHARED_REBOOTS}) ----",
                                test.name
                            );
                            qemu = boot(qemu.shutdown());
                            result = qemu.run_test(&test.qemu_name, test.timeout);
                        }
                        // Between the test and its check, with the guest still up:
                        // see [`TestDef::settle`].
                        (test.settle)(&mut qemu, &mut result);
                        let reason = (!(test.check)(&result)).then(|| {
                            result
                                .error
                                .as_ref()
                                .map(ToString::to_string)
                                // What the guest said rides the reason, as a machine
                                // test's capture does: an exit code is every
                                // assertion's.
                                .unwrap_or_else(|| {
                                    format!("exit code {:?}\n{}", result.exit_code, result.stdout)
                                })
                        });
                        done += 1;
                        send(test.name.clone(), reason, start);
                    }
                    free = qemu.shutdown();
                }
                Ok(())
            });
            if let Err(reason) = outcome {
                for test in &tests[done..] {
                    send(test.name.clone(), Some(reason.clone()), common::clock::mark());
                }
            }
        }
        Task::Machine(names) => {
            let carried = carried_by(&names, bins);
            // Dropped with the task, so no group's guest outlives the worker
            // that booted it.
            let mut held: Grouped = None;
            for name in names {
                let start = common::clock::mark();
                let outcome = catching(|| {
                    run_machine_test(name, bins.test_config, &carried.c, &carried.rust, &mut held)
                });
                // **A member that failed does not hand its guest on.** The
                // shared block answers a boot that stopped answering with a new
                // one; a group's guest is the same single point of failure and
                // that repair does not reach it, because a member's body is
                // arbitrary Rust with no `===TEST_START` for `started` to read.
                // What a group *does* have is a verdict per member, and one that
                // failed is reason enough not to make the next member's answer
                // about the same machine. Run `31250706113`: three members
                // behind one, `metal_sim_client_death` last and reported at
                // 364 s of a ceiling for a desktop that had gone.
                //
                // Bounded by the group — six members at most, so at most six
                // boots where there would have been one, and only on a red.
                if outcome.is_err() {
                    held = None;
                }
                send(name.to_string(), outcome.err(), start);
            }
        }
        Task::Screen(name) => {
            let carried = carried_by(&[name], bins);
            let start = common::clock::mark();
            let outcome =
                catching(|| run_screen_test(name, bins.test_config, &carried.c, &carried.rust));
            send(name.to_string(), outcome.err(), start);
        }
    }
}

impl Task<'_> {
    /// Every name this task will report an outcome for.
    fn names(&self) -> Vec<&str> {
        match self {
            Task::Shared(tests, _) => tests.iter().map(|t| t.name.as_str()).collect(),
            Task::Machine(names) => names.to_vec(),
            Task::Screen(name) => vec![name],
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
fn longest_first(tasks: &mut [Task<'_>], known: &BTreeMap<String, Duration>) {
    let cost = |task: &Task<'_>| -> Duration {
        task.names()
            .iter()
            .map(|n| known.get(*n).copied().unwrap_or(Duration::MAX))
            .fold(Duration::ZERO, |a, b| a.saturating_add(b))
    };
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
fn run_phase(
    tasks: Vec<Task<'_>>,
    width: usize,
    bins: &Bins<'_>,
) -> Vec<Outcome> {
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
                    run_task(task, bins, &tx);
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

/// The selected machine tests as boots: a run of adjacent names of one group is
/// one task.
fn machine_tasks(selected: &[(&'static str, Sched)]) -> Vec<(Sched, Vec<&'static str>)> {
    let mut out: Vec<(Sched, Vec<&'static str>)> = Vec::new();
    for &(name, sched) in selected {
        let joins = group_of(name).is_some()
            && out.last().is_some_and(|(_, names)| {
                group_of(names[names.len() - 1]) == group_of(name)
            });
        match out.last_mut() {
            Some((_, names)) if joins => names.push(name),
            _ => out.push((sched, vec![name])),
        }
    }
    out
}

/// Every test with a boot, split into the parallel and serial phases.
///
/// Pulled out of `main` so [`check_shard_partition`] builds the identical
/// lists a real run would rather than a second, hand-written approximation
/// that could pass its own check while the real path still disagreed with
/// itself — which is exactly the shape of the defect run `31617589126` found.
fn build_tasks<'a>(
    tests_to_run: &[&'a TestDef],
    machine_to_run: &[(&'static str, Sched)],
    screen_to_run: &[(&'static str, Sched)],
) -> (Vec<Task<'a>>, Vec<Task<'a>>) {
    let mut parallel: Vec<Task> = Vec::new();
    let mut serial: Vec<Task> = Vec::new();
    if !tests_to_run.is_empty() {
        let (actuator, shipping): (Vec<&TestDef>, Vec<&TestDef>) =
            tests_to_run.iter().copied().partition(|t| ACTUATOR_TESTS.contains(&t.name.as_str()));
        for tests in [shipping, actuator] {
            if tests.is_empty() {
                continue;
            }
            let features = shared_kernel(&tests[0].name);
            let task = Task::Shared(tests, features);
            match SHARED_BLOCK {
                Sched::Parallel => parallel.push(task),
                Sched::Serial => serial.push(task),
            }
        }
    }
    for (sched, names) in machine_tasks(machine_to_run) {
        let task = Task::Machine(names);
        match sched {
            Sched::Parallel => parallel.push(task),
            Sched::Serial => serial.push(task),
        }
    }
    for &(name, sched) in screen_to_run {
        let task = Task::Screen(name);
        match sched {
            Sched::Parallel => parallel.push(task),
            Sched::Serial => serial.push(task),
        }
    }
    (parallel, serial)
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
fn check_shard_partition(all_tests: &[TestDef]) {
    let pricing = shard_pricing();
    for reach in [Reach::Fast, Reach::Nightly, Reach::Weekly] {
        // Sharded, because every run this partition is for is one.
        let in_tier = |tier: Tier| tier.selected(reach, true);
        let tests_to_run: Vec<&TestDef> =
            all_tests.iter().filter(|_| in_tier(SHARED_TIER)).collect();
        let machine_to_run: Vec<(&str, Sched)> = MACHINE_TESTS
            .iter()
            .filter(|(_, _, tier)| in_tier(*tier))
            .map(|(n, s, _)| (*n, *s))
            .collect();
        let screen_to_run: Vec<(&str, Sched)> = SCREEN_TESTS
            .iter()
            .filter(|(_, _, tier)| in_tier(*tier))
            .map(|(n, s, _)| (*n, *s))
            .collect();

        let (parallel, serial) = build_tasks(&tests_to_run, &machine_to_run, &screen_to_run);
        // Owned, not borrowed: each shard below clones `parallel`/`serial` into
        // a scratch `Vec` that does not outlive its own loop iteration, so what
        // accumulates across iterations cannot hold a reference into it.
        let want: BTreeSet<String> =
            parallel.iter().chain(&serial).flat_map(Task::names).map(str::to_string).collect();

        const COUNT: usize = 12;
        let cost = |task: &Task<'_>| -> Option<Duration> {
            task.names().iter().try_fold(Duration::ZERO, |a, n| Some(a + *pricing.get(*n)?))
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
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
            for name in mine_p.iter().chain(&mine_s).flat_map(Task::names) {
                assert!(
                    seen.insert(name.to_string()),
                    "{reach:?}: {name} lands in shard {index}/{COUNT} and at least \
                     one earlier shard too — every execution label must belong to exactly one"
                );
            }
        }
        assert_eq!(
            seen, want,
            "{reach:?}: the twelve shards together do not equal the full selection — \
             {:?} present in the selection and missing from every shard",
            want.difference(&seen).collect::<Vec<_>>()
        );
    }
}

/// Every claim the three explicit registration lists make about themselves,
/// before anything boots.
///
/// A group whose members drifted apart still passes — each one boots its own
/// machine and reads its own console — so nothing downstream would notice, and
/// a group split across the two phases could not share a guest at all.
/// Every metal row names a registered test or a [`METAL_ONLY`] row — exactly
/// one of the two — once, and every boot it asks for is a committed config.
///
/// **A row for a name nothing registers is a metal test with no QEMU one**, and
/// its verdict would be reported under a name no other tier can answer for
/// unless a [`METAL_ONLY`] row says why none can.
fn check_metal_registration() {
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
    common::https::every_bench_claims_what_its_config_declares();
    if let Err(why) = the_metal_gates_refuse_what_they_name() {
        panic!("{why}");
    }
    let registered: BTreeSet<&str> = MACHINE_TESTS
        .iter()
        .chain(SCREEN_TESTS)
        .map(|(n, _, _)| *n)
        .collect();
    if let Err(why) = metal_rows_are_registered(&registered, METAL, METAL_ONLY) {
        panic!("{why}");
    }
}

/// [`check_metal_registration`]'s rule over any three tables, so the gate is
/// held to fixtures as well as to the tables it guards.
fn metal_rows_are_registered(
    registered: &BTreeSet<&str>,
    rows: &[(&str, metal::Metal)],
    metal_only: &[(&str, &str)],
) -> Result<(), String> {
    let only: BTreeSet<&str> = metal_only.iter().map(|(n, _)| *n).collect();
    if only.len() != metal_only.len() {
        return Err("METAL_ONLY names one test twice".to_string());
    }
    for (name, why) in metal_only {
        if why.trim().is_empty() {
            return Err(format!("METAL_ONLY declares {name:?} with no reason"));
        }
        if !rows.iter().any(|(n, d)| n == name && matches!(d, metal::Metal::Runs { .. })) {
            return Err(format!(
                "METAL_ONLY declares {name:?}, which no METAL row runs; a metal-only name with \
                 no metal arm is a test nothing runs"
            ));
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (name, decl) in rows {
        match (registered.contains(name), only.contains(name)) {
            (true, false) | (false, true) => {}
            (false, false) => {
                return Err(format!(
                    "METAL rules on {name:?}, which no registration names; a metal-only test \
                     is declared in METAL_ONLY with why no QEMU arm can answer for it"
                ))
            }
            (true, true) => {
                return Err(format!(
                    "{name:?} is registered and declared metal-only; a test with a QEMU arm is \
                     not one no QEMU arm can answer for"
                ))
            }
        }
        if !seen.insert(name) {
            return Err(format!("{name} has two metal declarations"));
        }
        let metal::Metal::Runs { arms, .. } = decl else { continue };
        if arms.is_empty() {
            return Err(format!("{name}'s metal declaration asks for no boot at all"));
        }
        for arm in *arms {
            let at = compile::repo_root().join(arm.config).join("system.toml");
            if !at.is_file() {
                return Err(format!("{name} boots {}, which holds no system.toml", arm.config));
            }
        }
    }
    Ok(())
}

/// **A [`METAL_ONLY`] name the shared boot answers under is a QEMU test hiding
/// behind a declaration that no QEMU arm answers for it**, and its metal verdict
/// would be reported beside the shared member's under one name. `shared` is
/// every name the shared registry discovers, Rust and C, and every name a
/// shared metal boot's members are recorded under.
fn metal_only_is_unshared(
    metal_only: &[(&str, &str)],
    shared: &BTreeSet<&str>,
) -> Result<(), String> {
    let clash: Vec<&str> =
        metal_only.iter().map(|(n, _)| *n).filter(|n| shared.contains(n)).collect();
    if !clash.is_empty() {
        return Err(format!(
            "METAL_ONLY declares {clash:?}, which the shared boot also answers for — two \
             verdicts under one name, and one of them a QEMU arm the declaration says cannot \
             exist. Rename the metal row."
        ));
    }
    Ok(())
}

/// [`metal_only_is_unshared`] against this process's discovered binaries and
/// the shared metal boots built from them, unfiltered.
fn check_metal_only_unshared(rust_bins: &[(String, Vec<u8>)], c_bins: &[(String, Vec<u8>)]) {
    let rust = discover_rust_tests(rust_bins);
    let mut boots = shared_metal(rust_bins, |_| true);
    boots.push(c_corpus_metal(c_bins, |_| true));
    let shared: BTreeSet<&str> = rust
        .iter()
        .map(String::as_str)
        .chain(c_bins.iter().map(|(n, _)| n.as_str()))
        .chain(boots.iter().flat_map(|b| &b.jobs).flat_map(|job| {
            [job.as_str(), job.strip_prefix("test_rs_").unwrap_or(job)]
        }))
        .collect();
    if let Err(why) = metal_only_is_unshared(METAL_ONLY, &shared) {
        panic!("{why}");
    }
}

/// Both metal gates, on fixtures: every refusal each one names is shown to
/// fire, and the tables it must accept are accepted.
fn the_metal_gates_refuse_what_they_name() -> Result<(), String> {
    fn judge(_: &[&metal::Readback]) -> Result<(), String> {
        Ok(())
    }
    const RUNS: metal::Metal = metal::Metal::Runs { arms: JOBCASE, judge };
    const NONE: metal::Metal = metal::Metal::Runs { arms: &[], judge };
    const QEMU_ONLY: metal::Metal = metal::Metal::QemuOnly("a fixture");
    const NO_CONFIG: metal::Metal = metal::Metal::Runs {
        arms: &[metal::once("nowhere", "tests/no-such-config", &[], &[])],
        judge,
    };
    let registered: BTreeSet<&str> = ["qemu_too"].into_iter().collect();
    const WHY: &str = "a fixture's reason";
    /// A fixture's name, its METAL rows, its METAL_ONLY rows, and the refusal
    /// it must draw, `None` for none.
    type Case = (
        &'static str,
        &'static [(&'static str, metal::Metal)],
        &'static [(&'static str, &'static str)],
        Option<&'static str>,
    );
    let cases: &[Case] = &[
        ("both kinds", &[("qemu_too", RUNS), ("metal", RUNS)], &[("metal", WHY)], None),
        ("a QEMU-only row", &[("qemu_too", QEMU_ONLY)], &[], None),
        ("an unregistered row", &[("stray", RUNS)], &[], Some("which no registration names")),
        (
            "a registered metal-only row",
            &[("qemu_too", RUNS)],
            &[("qemu_too", WHY)],
            Some("registered and declared metal-only"),
        ),
        (
            "a metal-only name twice",
            &[("metal", RUNS)],
            &[("metal", WHY), ("metal", WHY)],
            Some("names one test twice"),
        ),
        ("a blank reason", &[("metal", RUNS)], &[("metal", " ")], Some("with no reason")),
        ("no row", &[], &[("metal", WHY)], Some("which no METAL row runs")),
        (
            "a QEMU-only row declared metal-only",
            &[("metal", QEMU_ONLY)],
            &[("metal", WHY)],
            Some("which no METAL row runs"),
        ),
        (
            "a row twice",
            &[("qemu_too", RUNS), ("qemu_too", RUNS)],
            &[],
            Some("has two metal declarations"),
        ),
        ("no boot", &[("qemu_too", NONE)], &[], Some("asks for no boot at all")),
        ("no config", &[("qemu_too", NO_CONFIG)], &[], Some("holds no system.toml")),
    ];
    for (case, rows, only, refused) in cases {
        match (metal_rows_are_registered(&registered, rows, only), refused) {
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
    let shared: BTreeSet<&str> = ["abuse_connect_flood", "00_hello"].into_iter().collect();
    for (name, refused) in [("abuse_connect_flood", true), ("00_hello", true), ("metal", false)] {
        if metal_only_is_unshared(&[(name, WHY)], &shared).is_err() != refused {
            return Err(format!(
                "the shared-name gate {} a METAL_ONLY {name:?} beside the shared names \
                 {shared:?}",
                if refused { "accepted" } else { "refused" }
            ));
        }
    }
    Ok(())
}

fn check_registration() {
    check_metal_registration();
    let mut rows: BTreeSet<&str> = BTreeSet::new();
    for (test, _) in CARRIES {
        assert!(rows.insert(test), "CARRIES has two rows for {test}");
        assert!(
            MACHINE_TESTS.iter().chain(SCREEN_TESTS).any(|(name, _, _)| name == test),
            "CARRIES has a row for {test}, which no MACHINE_TESTS or SCREEN_TESTS entry registers"
        );
    }
    let mut groups: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for (i, (name, _, _)) in MACHINE_TESTS.iter().enumerate() {
        let Some(group) = group_of(name) else { continue };
        let span = groups.entry(group).or_insert((i, i, 0));
        span.1 = i;
        span.2 += 1;
    }
    for (group, (first, last, count)) in groups {
        assert_eq!(
            last - first + 1,
            count,
            "{group}'s members are not adjacent in MACHINE_TESTS, so they cannot share a boot"
        );
        assert!(
            MACHINE_TESTS[first..=last].windows(2).all(|w| w[0].1 == w[1].1),
            "{group} shares one boot, so its members must share one scheduling answer"
        );
        // **One boot cannot be in two tiers.** A group whose members disagreed
        // would put the boot in the fast tier for whichever member ran first
        // and charge the fast tier the whole group's cost.
        assert!(
            MACHINE_TESTS[first..=last].windows(2).all(|w| w[0].2 == w[1].2),
            "{group} shares one boot, so its members must share one tier"
        );
    }
}

/// Every name this suite can produce a verdict for, at its one tier: the shared
/// boot's discovered binaries and the two declared registries, so a name two
/// of them give is refused before anything boots.
fn schedule(shared: &[TestDef]) -> Schedule<'_> {
    Schedule::new(
        shared
            .iter()
            .map(|t| (t.name.as_str(), SHARED_TIER))
            .chain(SCREEN_TESTS.iter().chain(MACHINE_TESTS).map(|(n, _, tier)| (*n, *tier))),
    )
    .unwrap_or_else(|refusal| {
        panic!(
            "{refusal}. A binary a machine test drives goes on RUST_SKIP with the reason its own \
             test exists, or one of the two is renamed."
        )
    })
}

/// `redlist::DISABLED` against every name the schedule holds, before any boot
/// on any entry point.
fn check_redlist(schedule: &Schedule<'_>) -> Result<(), String> {
    redlist::check(redlist::DISABLED, |name| schedule.contains(name), &compile::repo_root())
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
    let keep = |name: &str| {
        filter.is_none_or(|f| name.contains(f)) && redlist::disabled(redlist::DISABLED, name).is_none()
    };

    let debug_mode = SUITE.present(&args, &testargs::DEBUG);
    let list_mode = SUITE.present(&args, &testargs::LIST);
    // How far down the tiers this run reaches. Flags and not an env var: an env
    // var is invisible in the command line and easy to leave set, and the whole
    // point of the split is that a run says what it ran.
    let reach = Reach::of(&args);
    let nocapture =
        SUITE.present(&args, &testargs::NOCAPTURE) || SUITE.present(&args, &testargs::SHOW_OUTPUT);

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

    if let Some(image) = SUITE.value(&args, &testargs::HOLD) {
        common::orphan::hold(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testcases"),
            Path::new(image),
        );
        run.exit(0);
    }

    check_registration();

    if nocapture || debug_mode {
        common::qemu::VERBOSE.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    // **The metal profile, before the C corpus.** It boots the T14 and not a
    // guest, so none of the C compile, the HTTPS judge's hosts or the tier
    // arithmetic below is any of its business; running it here is what keeps a
    // `--metal` invocation costing a kernel and a userland and nothing else.
    if let Some(mode) = parsed.metal {
        let rust_tests_dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/toyos-rust-tests");
        eprintln!("[toyos] Building Rust tests...");
        let rust_bins = qemu::build_toyos_bins(&rust_tests_dir);
        // The C corpus is compiled here rather than below, because the metal
        // branch returns before the ordinary suite's own compile — and a boot
        // that carries the corpus needs the binaries beside their expectations.
        let c_names = discover_c_tests();
        eprintln!("[toyos] Compiling {} C tests for the corpus boot...", c_names.len());
        let c_bins = compile_c_tests(&c_names);
        check_metal_only_unshared(&rust_bins, &c_bins);
        let c_compiled: Vec<String> = c_bins.iter().map(|(n, _)| n.clone()).collect();
        if let Err(refusal) = check_redlist(&schedule(&build_test_registry(&rust_bins, &c_compiled))) {
            eprintln!("[toyos] src/redlist.rs: {refusal}");
            run.exit(1);
        }
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
        let mut boots = shared_metal(&rust_bins, keep);
        boots.push(c_corpus_metal(&c_bins, keep));

        // Three statuses for the three things this can establish, as the
        // ordinary suite has: green, red, and "measured nothing" — a run that
        // staged images and never reached the machine has no claim to make.
        run.exit(
            match metal::run(mode, &selected, &boots, &rust_bins, RUST_SKIP, !nocapture && !debug_mode)
            {
                metal::Verdict::Green => 0,
                metal::Verdict::Red => 1,
                metal::Verdict::Staged => 2,
            },
        );
    }

    let c_names = discover_c_tests();
    eprintln!(
        "[toyos] Compiling {} C tests, and attempting {} declared ones...",
        c_names.len(),
        NOT_RUN.len()
    );
    check_not_run();
    let c_bins = compile_c_tests(&c_names);
    let c_compiled: Vec<String> = c_bins.iter().map(|(n, _)| n.clone()).collect();

    let rust_tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/toyos-rust-tests");
    eprintln!("[toyos] Building Rust tests...");
    let rust_bins = qemu::build_toyos_bins(&rust_tests_dir);
    toyos_build::build::build_host_judges(&common::compile::repo_root(), !nocapture && !debug_mode);

    // Every name this process could produce a verdict for, before `--list`
    // and `--debug` can return without ever reaching it.
    let all_tests = build_test_registry(&rust_bins, &c_compiled);
    let schedule = schedule(&all_tests);
    if let Err(refusal) = check_redlist(&schedule) {
        eprintln!("[toyos] src/redlist.rs: {refusal}");
        run.exit(1);
    }

    // Every row against the catalogue before `--list` and before any boot, so a
    // name the suite did not build is refused here and not by whichever worker
    // reaches it.
    for (_, names) in CARRIES {
        qemu::carrying(&c_bins, &rust_bins, names.iter().copied());
    }

    // --list: every test at its tier, and exit
    if list_mode {
        for (name, tier) in schedule.iter() {
            println!("{tier:?} {name}");
        }
        return;
    }

    if debug_mode {
        run_debug_mode(&c_bins, &rust_bins);
        return;
    }

    check_metal_only_unshared(&rust_bins, &c_bins);
    check_shard_partition(&all_tests);

    // A name filter reaches every tier; a shard still excludes `Tier::Local`.
    let in_tier = |tier: Tier| tier.selected(reach.for_filter(filter.is_some()), shard.is_some());
    let tests_to_run: Vec<&TestDef> = all_tests
        .iter()
        .filter(|t| keep(t.name.as_str()) && in_tier(SHARED_TIER))
        .collect();
    let screen_to_run: Vec<(&str, Sched)> = SCREEN_TESTS
        .iter()
        .filter(|(n, _, tier)| keep(n) && in_tier(*tier))
        .map(|(n, s, _)| (*n, *s))
        .collect();
    let machine_to_run: Vec<(&str, Sched)> = MACHINE_TESTS
        .iter()
        .filter(|(n, _, tier)| keep(n) && in_tier(*tier))
        .map(|(n, s, _)| (*n, *s))
        .collect();

    // **What this run is not doing, said before it does anything.** A run that
    // quietly does less than the last one is the failure mode the tier
    // introduces, so the names are printed rather than counted, and the line
    // carries both the command that runs them and the record that says what each
    // one guarded.
    let held = |which: Tier| -> Vec<String> {
        schedule
            .iter()
            .filter(|(n, tier)| keep(n) && *tier == which && !in_tier(*tier))
            .map(|(n, _)| n.to_string())
            .collect()
    };
    let held_local = held(Tier::Local);
    if !held_local.is_empty() {
        eprintln!(
            "[toyos] local tier: {} test(s) NOT run, because a sharded run is CI's and no CI \
             runner boots their architecture yet. An unsharded `cargo test` runs them.",
            held_local.len(),
        );
        eprintln!("[toyos]   {}", held_local.join(", "));
    }
    let held_back: Vec<(Tier, Vec<String>)> =
        [Tier::Nightly, Tier::Weekly].into_iter().map(|tier| (tier, held(tier))).collect();
    for (tier, names) in held_back.iter().filter(|(_, names)| !names.is_empty()) {
        let flag = tier.flag().expect("a held tier is a reach's");
        eprintln!(
            "[toyos] {} test(s) NOT run without {flag}, which \
             `cargo test --test toyos-build -- {flag}` and .github/workflows/nightly.yml's \
             schedule for it run.",
            names.len(),
        );
        eprintln!("[toyos]   {}", names.join(", "));
    }

    if tests_to_run.is_empty()
        && screen_to_run.is_empty()
        && machine_to_run.is_empty()
    {
        eprintln!("No enabled test matches filter {filter:?}");
        run.exit(1);
    }

    let test_config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testcases");
    let mut tally = Tally::new().holding_back(held_back);

    let suite_start = common::clock::mark();

    let bins = Bins {
        test_config: &test_config,
        c_bins: &c_bins,
        rust_bins: &rust_bins,
    };

    // Everything that owns a boot, split by the one question its registration
    // asks. Dispatch is declaration order and the queue is FIFO, so
    // MACHINE_TESTS keeping the plain-kernel names first and SCREEN_TESTS
    // putting its feature-carrying ones last still holds inside each phase.
    //
    // No longest-first heuristic, deliberately: the phase's wall clock is set by
    // its longest job and the durations that would order it are not in the
    // tree.
    if !tests_to_run.is_empty() {
        let actuator_count =
            tests_to_run.iter().filter(|t| ACTUATOR_TESTS.contains(&t.name.as_str())).count();
        eprintln!(
            "[toyos] The shared boots run {} of {} C + {} Rust binaries: {} on the shipping \
             kernel, {} on the actuator one",
            tests_to_run.len(),
            c_bins.len(),
            rust_bins.len(),
            tests_to_run.len() - actuator_count,
            actuator_count,
        );
    }
    let (mut parallel, mut serial) = build_tasks(&tests_to_run, &machine_to_run, &screen_to_run);

    let known = load_durations();
    // After the phases are decided and before either is ordered: what a shard
    // divides is the work, and a task's answer to `Sched` is a property of the
    // test rather than of how many machines are running it.
    if let Some(shard) = shard {
        // [`shard_pricing`], and not `known`: every process partitioning the
        // same run must price a task identically, which only the committed
        // profile guarantees.
        let pricing = shard_pricing();
        // A task whose every name has been timed costs their sum; one carrying
        // a name the profile has never seen is unmeasured, which is the same
        // all-or-nothing rule [`longest_first`] states with `Duration::MAX`.
        let cost = |task: &Task<'_>| -> Option<Duration> {
            task.names()
                .iter()
                .try_fold(Duration::ZERO, |a, n| Some(a + *pricing.get(*n)?))
        };
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
    let total = parallel.iter().chain(serial.iter()).map(|t| t.names().len()).sum::<usize>();
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
        let outcomes = run_phase(parallel, width, &bins);
        eprintln!("  --- parallel done in {:.1?} ---", started.elapsed());
        timed.extend(outcomes.iter().map(|o| (o.name.clone(), o.elapsed)));
        outcomes.into_iter().for_each(|o| tally.record(o));
    }
    if !serial.is_empty() {
        eprintln!("  --- serial ---");
        let started = std::time::Instant::now();
        let outcomes = run_phase(serial, 1, &bins);
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
    // A green run is a claim that this tree passed, and `--land`'s gate consumes
    // exactly this number. A run that spanned a suspend did not establish that:
    // its liveness ceilings were measured on a clock that stopped with it, so
    // exit 0 would be a claim it cannot support.
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

/// `rootfs::MOUNTED_FROM_MEMORY`: the kernel mounted ROOT off the loader's image.
const ROOT_MOUNTED_FROM_MEMORY: &str = "root: mounted read-only from memory at";
/// `rootfs::INIT_WITHOUT_A_DISK`, followed by the count of storage commands.
const INIT_WITHOUT_A_DISK: &str = "boot: init spawned with ROOT from memory; storage commands before it:";
/// `toyos_abi::boot::WITHHOLD_ROOT_PARAM`.
const ROOT_WITHHELD_PARAM: &str = toyos_abi::boot::WITHHOLD_ROOT_PARAM;
/// The kernel's refusal of a handoff that carries no ROOT image.
const ROOT_WITHHELD_REFUSAL: &str =
    "boot: the loader handed no ROOT image, and this kernel reads ROOT from memory and nowhere else";

/// ROOT came from memory: the kernel's mount record names the image, and the
/// record at init's spawn counts zero storage commands before it — every NVMe
/// command and every USB mass-storage command counts, so a ROOT read off a disk
/// could not leave it at zero. Both precede the first storage driver's line.
fn root_from_memory(log: &str) -> Result<(), String> {
    let mounted = log
        .find(ROOT_MOUNTED_FROM_MEMORY)
        .ok_or_else(|| format!("no {ROOT_MOUNTED_FROM_MEMORY:?} record in the boot log"))?;
    let spawned = log
        .find(INIT_WITHOUT_A_DISK)
        .ok_or_else(|| format!("no {INIT_WITHOUT_A_DISK:?} record in the boot log"))?;
    let count = log[spawned + INIT_WITHOUT_A_DISK.len()..]
        .lines()
        .next()
        .map(str::trim)
        .ok_or("the spawn record carries no count")?;
    if count != "0" {
        return Err(format!("init was spawned after {count} storage command(s), wanted none"));
    }
    if mounted > spawned {
        return Err("the ROOT mount record follows init's spawn".to_string());
    }
    for driver in ["nvme:", "NVMe:", "usb-storage:", "gpt: device"] {
        if let Some(at) = log.find(driver) {
            if at < spawned {
                return Err(format!("{driver:?} spoke before init's spawn record"));
            }
        }
    }
    eprintln!("  [root] mounted from memory, init spawned with 0 storage commands before it");
    Ok(())
}

const LOADER_TSC: &str = "Loader TSC: ";
/// The kernel's line, followed by its four spans in milliseconds.
const POWER_ON: &str = "boot: power-on to loader ";
/// The kernel's `TSC:` record, followed by the period it calibrated.
const TSC_PERIOD: &str = "MHz (period=";

/// **The boot from power-on is the loader's raw counts at the kernel's rate.**
/// The kernel's first two spans are the loader's counts converted at the `TSC:`
/// record's period, the ROOT read sits inside the loader's span, and
/// `Boot: complete`'s own count inside the kernel's.
fn boot_from_power_on(log: &str) -> Result<(), String> {
    let after = |head: &str| -> Result<Vec<u128>, String> {
        let at = log.find(head).ok_or_else(|| format!("no {head:?} line in the boot log"))?;
        let line = log[at + head.len()..].lines().next().unwrap_or("");
        Ok(line
            .split(|c: char| !c.is_ascii_digit())
            .filter(|word| !word.is_empty())
            .map(|word| word.parse().expect("a run of digits"))
            .collect())
    };
    let (loader, spans, rate) = (after(LOADER_TSC)?, after(POWER_ON)?, after(TSC_PERIOD)?);
    let (&[entry, handoff, ..], &[to_loader, in_loader, root_read, to_complete], &[period_fs, ..]) =
        (&loader[..], &spans[..], &rate[..])
    else {
        return Err(format!(
            "the lines do not carry their numbers: {loader:?} after {LOADER_TSC:?}, {spans:?} after \
             {POWER_ON:?}, {rate:?} after {TSC_PERIOD:?}"
        ));
    };
    let ms = |ticks: u128| ticks * period_fs / 1_000_000_000_000;
    if (to_loader, in_loader) != (ms(entry), ms(handoff - entry)) {
        return Err(format!(
            "the kernel says {to_loader} ms to the loader and {in_loader} ms in it; the loader's \
             counts {entry} and {handoff} at {period_fs} fs a tick are {} and {}",
            ms(entry),
            ms(handoff - entry)
        ));
    }
    if root_read > in_loader {
        return Err(format!("the ROOT read took {root_read} ms of a loader that took {in_loader}"));
    }
    let complete = after("Boot: complete (")?;
    if complete.first().is_none_or(|&own| own > to_complete) {
        return Err(format!(
            "`Boot: complete` counts {complete:?} ms from its own start, inside a kernel span the \
             power-on line puts at {to_complete} ms"
        ));
    }
    eprintln!(
        "  [boot] power-on to loader {to_loader} ms, loader {in_loader} ms (ROOT read {root_read} \
         ms), kernel {to_complete} ms; IA32_TSC_ADJUST {}",
        log.split("IA32_TSC_ADJUST ").nth(1).and_then(|rest| rest.lines().next()).unwrap_or("unsaid")
    );
    Ok(())
}

/// `toyos_abi::boot::WRITE_NO_LAYOUT_PARAM`.
const LAYOUT_ZERO_PARAM: &str = toyos_abi::boot::WRITE_NO_LAYOUT_PARAM;
/// The kernel's refusal of a `KernelArgs` layout word of 0, up to the layout
/// it reads.
const LAYOUT_REFUSAL: &str = "boot: the loader wrote KernelArgs layout 0x0 and this kernel reads layout 0x";
/// `blackbox::arm`'s record, the kernel's first read of a field after the word.
const BLACK_BOX_ARMED: &str = "black box: ";

/// A loader that wrote another layout is a boot refused by name, before the
/// kernel read the boot parameter. Checked against the kernel's own word,
/// `toyos_abi::boot::LAYOUT` in hex, and not merely the message's prefix: a
/// kernel that printed its own `kernel_args.layout` instead would still be 0x0
/// and still match the prefix.
fn kernel_args_layout_refused(log: &str) -> Result<(), String> {
    let refusal = format!("{LAYOUT_REFUSAL}{:x}", toyos_abi::boot::LAYOUT);
    if !log.contains(&refusal) {
        return Err(format!("no {refusal:?} in the boot log"));
    }
    if log.contains(BLACK_BOX_ARMED) {
        return Err(format!("a boot refused its layout still said {BLACK_BOX_ARMED:?}"));
    }
    eprintln!("  [boot] a loader that wrote layout 0 was refused by name before the boot parameter");
    Ok(())
}

/// A loader that hands no ROOT image is a boot refused by name: the kernel's
/// refusal is on the console, and nothing after it mounted ROOT from anywhere.
fn root_withheld_refused(log: &str) -> Result<(), String> {
    if !log.contains(ROOT_WITHHELD_REFUSAL) {
        return Err(format!("no {ROOT_WITHHELD_REFUSAL:?} in the boot log"));
    }
    for never in [ROOT_MOUNTED_FROM_MEMORY, INIT_WITHOUT_A_DISK, "Boot: storage ready"] {
        if log.contains(never) {
            return Err(format!("a boot handed no ROOT image still said {never:?}"));
        }
    }
    eprintln!("  [root] a handoff with no ROOT image refused the boot by name");
    Ok(())
}
