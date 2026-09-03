//! Which tests are known to go red, on which instrument, at what rate, and on
//! whose evidence — as data rather than as prose.
//!
//! **The failure this exists to stop.** The list used to be paragraphs — in
//! `issues/hardware/eleven-names-red-on-ci.md` and in a CI assessment since
//! deleted — and a careful reader can read a paragraph backwards: `rg`ing a test
//! name in it hits the sentence that names the twelve tests that came *off* the
//! list as readily as the table of the eleven that are on it, and the hit looks
//! the same either way. That happened, and the answer given to the owner was the
//! opposite of what the document said. **A list a grep can invert is not a
//! list.**
//!
//! **The shape.** A row is *one measurement*, never "a test". One name can carry
//! several — `xhci_hid_break` is 0 of 5 in one probe and red twice on `main`
//! since, on two different failures — and a schema keyed on the test name would
//! have to pick one of them to be the truth. [`Finding`] is what a measurement
//! said and [`Standing`] is whether anything has retired it; the two are
//! orthogonal, and neither is a sentence.
//!
//! **Why a zero cannot read as a red.** [`Finding::Quiet`] has no numerator: it
//! is not a `Fires` with a zero in it, it is a different variant, and
//! [`Finding::fires`] refuses a zero at compile time. So the row that says "this
//! came off the list" cannot be rendered, grepped or destructured into the row
//! that says "this reds".
//!
//! **Ask it, do not read it**: `cargo run -- --known-red <test>` prints every
//! row for one name, newest first, each with its instrument, its rate, its run
//! and the day it was taken; with no argument it prints the whole index one line
//! per name. A name with no rows answers `NOT ON THE LIST`, which is a claim
//! that nothing here has measured it and not a claim that it is green.
//!
//! **What this is not.** `EXPECTED_FAILURES` in `tests/toyos.rs` is a
//! *declaration*: a named red, with a task and a write-up, that makes a run exit
//! 0. This index declares nothing and exempts nothing — every row is a red that
//! is still a red, and a run that hits one is still red. The two overlap on
//! `hda_tone`, at two different assertions, and the query says so where they do.
//!
//! **The bound on honesty, stated plainly.** Nothing here watches a test run, so
//! no row can detect its own fix the way `Stale::OnAPass` does in
//! `tests/toyos.rs` — a rate is not falsified by one green, which is exactly why
//! that mechanism concedes a date for its intermittents. What this has instead is
//! [`SHELF_LIFE_DAYS`]: every row that still stands carries the day it was
//! measured, and a month after it the gate below reds. **The cheap honest
//! response to that red is to delete the row.** An observation nobody will
//! re-measure is not something anyone should be trusting, and an index that
//! shrinks to nothing is a true statement about how much is known.
//!
//! **A `Red::source` may point at a code site as readily as a write-up.**
//! Retiring a row against the commit that fixed it means repointing its source
//! at the site that now enforces the rule — never leaving it aimed at a deleted
//! file, which `every_row_can_say_what_it_claims` refuses.
//!
//! **What is deliberately not a row.** Gate A's thorough tier compares
//! distributions against a recorded sample (`tests/audio-baseline.toml`) and its
//! verdicts are `Fisher p=…`, not "this test went red"; those live with the
//! baseline. Metal is not an instrument here either — not because the suite
//! skips the T14 (it runs there daily, since `985f3834`), but because it runs
//! there in QEMU under KVM, which is [`Ci`], not bare hardware.
//!
//! [`Ci`]: Instrument::Ci

use crate::day::Day;
use std::collections::BTreeSet;
use std::path::Path;

/// Which machine took the measurement. A row without this is a row that will be
/// read as being about whichever machine the reader is standing at.
///
/// **Every defect class has exactly one owning instrument, and the owner's red
/// is that class's alarm.** Four instruments carry the whole estate, and each is
/// blind to something another sees: host suites own pure logic — decoders,
/// validators, layouts, the build system's own gates, and the memory orderings
/// x86 TSO hides — and are blind to anything needing a booted kernel; KVM guest
/// shards own the booted kernel on native silicon and audible harm, and are
/// blind to contention and to whatever the hypervisor absorbs; the TCG shard
/// owns ISA breadth, the instruction paths the KVM hosts' CPUs never decode, and
/// is blind to vendor-real semantics and to realistic instruction cost; metal
/// owns what emulation absorbs — cache and control-register effects, PAT/MTRR,
/// device timing, real latency — and is blind to anything needing repetition or
/// isolation, being one manual machine. A defect found by a non-owning
/// instrument transfers to its owner. Only three of the four can appear below:
/// **metal is not an instrument here**, because nothing below is ToyOS on bare
/// hardware. The T14 does run the suite — nightly for gate A, and on any
/// dispatch — but it runs it in QEMU under KVM, which is [`Ci`].
///
/// [`Ci`]: Instrument::Ci
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub enum Instrument {
    /// A `guest` lane: KVM on native x86-64 cores, `-cpu host`, **one guest per
    /// machine**, `--jobs 1`, nothing else on the box.
    ///
    /// **Two machines wear this name and they do not price alike.**
    /// `.github/workflows/route.yml` sends every event to twelve GitHub-hosted
    /// shards of four EPYC cores each except a `workflow_dispatch` and a
    /// `schedule` that is not `ci.yml`'s; those two get one 1/1 lane on the
    /// T14's i5-1135G7. A row has to say which in its [`Red::evidence`],
    /// because the difference is not noise: one tip measured
    /// `xhci_full_speed_device` at 6,845 ms in the first and 12,156 ms in the
    /// second on one day (`src/durations.rs` carries the measurement, and the
    /// committed profile's `shards=` column records which partition took each
    /// price).
    /// Rows measured on a pull request or a push between 2026-08-21 and
    /// 2026-08-22 were taken on the T14 under the routing of those days — a
    /// fact about which machine that row's number came from, not a reason to
    /// discount it.
    Ci,
    /// The dev host with the test run by itself. Cross-arch TCG on arm64.
    DevHostAlone,
    /// The dev host in the wide phase, or under another worktree's suite or
    /// build. The only instrument that can produce a contention red at all.
    ///
    /// **Two guests failing in one phase is not by itself a claim about the
    /// host.** Simultaneity only argues for a common cause while no per-guest
    /// mechanism has a rate; once one does, the arithmetic decides. At the
    /// direction-flag class's measured 37 silent deaths in 13,960 loaded boots,
    /// a suite of ~140 boots pays `P(>=2) ~ 5%` — so a pair in one run is an
    /// ordinary coincidence, and reading it as a host-level event is a
    /// conclusion the evidence never supported.
    DevHostLoaded,
}

impl Instrument {
    fn label(self) -> &'static str {
        match self {
            Instrument::Ci => "CI",
            Instrument::DevHostAlone => "dev host, alone",
            Instrument::DevHostLoaded => "dev host, loaded",
        }
    }

    /// What a verdict from this instrument cannot be about.
    fn cannot_say(self) -> &'static str {
        match self {
            Instrument::Ci => {
                "one guest per machine, so nothing here is about contention — the dev \
                 host's whole ALONE: GREEN class is invisible to it"
            }
            Instrument::DevHostAlone | Instrument::DevHostLoaded => {
                "cross-arch TCG, so nothing here is about which vendor's reading of an \
                 instruction the kernel depends on"
            }
        }
    }
}

/// What the measurement said. Three shapes, and they are not one shape with a
/// number in it.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Finding {
    /// It fired, at a rate. Build one with [`Finding::fires`].
    Fires { red: u32, of: u32 },
    /// It was measured `of` times and **did not fire once**. There is no
    /// numerator to misread. Build one with [`Finding::quiet`].
    Quiet { of: u32 },
    /// It fired, and nothing here is a rate — one run, or runs nobody counted.
    Seen,
}

impl Finding {
    /// `red` of `of` runs of one thing — one probe's reps, one branch's runs,
    /// one session's suites. [`Red::evidence`] says which.
    ///
    /// The three rules are compile errors rather than a test, because a `const`
    /// item's initialiser is evaluated on the way to the binary: a zero
    /// numerator is [`Finding::quiet`] and must not be able to wear this shape,
    /// one run is not a rate, and more reds than runs is a typo.
    pub const fn fires(red: u32, of: u32) -> Finding {
        assert!(
            red > 0,
            "a Fires row with no reds is a Quiet row, and the two must not be one shape"
        );
        assert!(
            of >= 2,
            "one run is not a rate — Finding::Seen is the shape for a single sample"
        );
        assert!(red <= of, "more reds than runs");
        Finding::Fires { red, of }
    }

    /// `of` runs of one thing, none of which fired.
    pub const fn quiet(of: u32) -> Finding {
        assert!(
            of >= 2,
            "one green is one sample of a rate and retires nothing — Finding::Seen"
        );
        Finding::Quiet { of }
    }

    /// Whether this measurement is a red at all.
    fn is_red(self) -> bool {
        matches!(self, Finding::Fires { .. } | Finding::Seen)
    }

    fn rendered(self) -> String {
        match self {
            Finding::Fires { red, of } => format!("FIRES {red} of {of}"),
            Finding::Quiet { of } => format!("QUIET 0 of {of}"),
            Finding::Seen => "SEEN  no rate".to_string(),
        }
    }
}

/// Whether anything has retired the measurement — orthogonal to what it said.
///
/// A `Fires` that is `Retired` is history and never a live red; a `Fires` that
/// `Stands` is the only thing that makes a name known-red.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Standing {
    /// Nothing has retired it.
    Stands,
    /// A landed fix or a later measurement retired it. Kept rather than deleted,
    /// because the next red under this name must not be read as this one.
    Retired(&'static str),
    /// **The sources disagree about whether it still stands, and this says how.**
    /// A disputed row never makes a name known-red by itself and never silences
    /// one either: the query prints the disagreement and a human decides. A row
    /// that overstates its confidence is worse than no row.
    Disputed(&'static str),
}

/// One measurement of one test on one instrument.
#[derive(Clone, Copy)]
pub struct Red {
    /// The registered test name, exactly. A renamed or deleted test takes its
    /// rows with it — the gate below refuses a name no list registers.
    pub test: &'static str,
    pub instrument: Instrument,
    pub finding: Finding,
    pub standing: Standing,
    /// What the failure said, quoted from the run wherever the source quotes it.
    pub what: &'static str,
    /// The run, probe or session this is a measurement of: a CI run id, a log
    /// file, a tree.
    pub evidence: &'static str,
    /// The write-up that carries the reasoning and the rest of the evidence, as
    /// a repository path optionally followed by a section. It must exist and it
    /// must name [`Red::test`] — a pointer that resolves to a document which has
    /// stopped being about this test is how the index and the prose drift apart.
    pub source: &'static str,
    /// `YYYY-MM-DD`, the day the measurement was taken — not the day the row was
    /// written. Every answer prints how long ago that was.
    pub measured: &'static str,
}

/// How long a standing row is worth anything without being re-taken.
///
/// A month, and the number is not invented: `tests/toyos.rs`'s two
/// `EXPECTED_FAILURES` entries use exactly this interval with exactly this
/// justification — long enough that a fix already in flight lands first, short
/// enough that nobody inherits it silently. The tree it is measured against
/// bears it out: nine consecutive `ci` runs on `main` over that window, one of
/// them green.
pub const SHELF_LIFE_DAYS: i64 = 31;

/// What retired both `screen_pager_keys` rows about the page-move count.
///
/// **The verdict was the defect, and the rows are the evidence for that**, which
/// is why they are retired rather than deleted: two agents read this arithmetic
/// as a kernel regression and bisected it to a merge, and a reader who meets the
/// same message needs to find that here rather than repeat it.
const PAGER_ARITHMETIC: &str = "the verdict was the defect. It injected all thirty keys at the \
    host's own speed and compared the moves it saw against what the pager's 3 s unattended \
    deadline could have produced in the elapsed time, so a host that got through the thirty in \
    0.3 s was asked for 3.3 moves and reported `0 page moves over 30 keystrokes` — the symptom of \
    a guest that had not been given time to repaint once. Unpaced it was wrong about the wire too: \
    thirty press/release pairs is sixty scancodes into QEMU's 16-byte `PS2_QUEUE_SIZE`, so keys a \
    full-panel repaint had no room for were never delivered. The verdict is now one key, then its \
    page, then the next key, with a guest-budgeted wait and no host clock in it, and the \
    unattended window is measured on the guest before the first key retires it. Re-measured \
    2026-08-24: PASS 6 of 6 alone on the dev host at 2.00x-8.00x width, 16-31 s each, against \
    every recorded red's 6-10 s";

const TYPING_UNATTRIBUTED: &str = "`shell_type_once` no longer sends a burst \
    the guest has not acknowledged, so the harness cannot provoke this again — but that is \
    avoidance, not attribution. This row's verdict is ten typed lines with no whole echo, and \
    `issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md` records that an \
    ordinary queue overflow shortens one line and the next recovers, while an input path that \
    stops explains ten of ten — naming both locale sightings as that shape. The counter that \
    separates bytes never delivered from a guest that stopped reading was recorded by neither \
    sighting, so the pacing fix accounts for a defect that is real and may not be this one's \
    cause. Disputed rather than retired: the name is not known-red on this row alone, and this \
    row does not silence it either. Settled by reproducing the ten-of-ten verdict with \
    `i8042-trace` armed and reading whether the drain count keeps rising after the first \
    failed line";

/// What retired both `xhci_slow_connect` rows: a later measurement, not a fix.
///
/// The number in the second row was the whole finding, and it no longer holds.
const SLOW_CONNECT_CLEARS: &str = "a later measurement. Re-run four times alone on the dev host \
    on 2026-08-24, the controller starts at 0.109, 0.117, 0.122 and 0.227 s against the 300 ms \
    held-empty window — 73 to 191 ms of clearance, not the 1 ms the row is about — on hosts the \
    harness independently measured at 2.31x to 4.45x width, and every boot bound both sticks and \
    named its first port at 0.400-0.418 s. The loaded-host half is answered structurally rather \
    than by the number: `src/tiers.rs` relegates this name `Why::TimerAnchored`, because a slower \
    machine changes its verdict rather than its price";

/// What retired all three `xhci_hid_break` endpoint-count rows, written once
/// because it is one landing and not three: three measurements of one
/// assertion, and repeating the sentence is how two of them would drift.
const HID_SCOPED: &str = "the count is per device now. The verdict pairs each broken completion \
    with the device it names — `hid_broke_on` reads `<bdf> slot <n>` off it, because a slot id is \
    one controller's numbering and this machine has two — and requires `endpoint 3 is Running, \
    recovering` exactly once for each of the two, so the boot disk's own bulk endpoints are no \
    longer counted and a missing recovery still reds. A completion that stops naming its device \
    is refused rather than widened back to every dci 3 on the machine";

/// Every measurement, grouped by the campaign that took it.
///
/// Adding a row means answering all eight fields; there is no default and no
/// abbreviation. Retiring one means saying what retired it. Deleting one is
/// always allowed and is what an unmaintainable row should get.
pub const KNOWN_RED: &[Red] = &[
    // ---------------------------------------------------------------------
    // `probe-rate.yml` run 31258202923, tree f8f73e1, 2026-08-08: five reps of
    // the exact twelve-shard configuration `ci.yml` runs, sixty jobs, 292 tests
    // each, 1460 outcomes. 281 of the 292 names were green in all five.
    // ---------------------------------------------------------------------
    // The budget is soft where the harness asserts it hard: eviction never takes
    // a dirty page, so an all-dirty resident set truthfully prints over-budget.
    Red {
        test: "cache_eviction",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the bound is asserted against the derivation, 2026-08-28. Eviction gives up only \
             when every governed resident page is dirty, so the turnover line now carries its \
             dirty count and says the over-budget state once per episode \
             (`kernel/src/file_cache.rs`), the guest rewrites eight pages through a second file \
             while the whole budget is dirty — the all-dirty overage staged on every run, where \
             CI met it by another writer's un-flushed page — and the harness admits an \
             over-budget sample only when its own line says dirty == resident, requires at least \
             one such sample, and requires the last sample back within the bound once the \
             writers flushed. A clean overage still reds. Control measured both ways: the staged \
             overage reds the old hard assertion with this row's exact message and greens the \
             derivation assertion",
        ),
        what: "file cache: 65 entries resident against a 64 bound after 1280 evictions — \
               the bound does not hold",
        evidence: "ci.yml run 33159606357 guest (3); green ALONE in the same session",
        source: "tests/toyos.rs",
        measured: "2026-08-28",
    },
    Red {
        test: "usb_transport_break",
        instrument: Instrument::Ci,
        finding: Finding::fires(5, 5),
        standing: Standing::Retired(
            "a driver defect, and it took both instruments: a Bulk-Only Reset issued while \
             the device could still answer the transfer it was recovering from. \
             `probe-xhci-break.yml` run 31264371902, control 3 of 3 red and fixed 3 of 3 green",
        ),
        what: "the transport broke 2 times; the injection is armed once per boot, so anything \
               else is a break this test did not stage",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    // ---------------------------------------------------------------------
    // A different assertion (`breaks > 2`, not the retired `breaks != 1`) on the
    // same test, `1cb11e7`'s re-issue budget. The injected disk never left its
    // two-break budget; the third line was the boot stick's own unrelated,
    // cleanly-recovered transport break, which the count did not scope out.
    // ---------------------------------------------------------------------
    Red {
        test: "usb_transport_break",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the count is per device now. `broke_on` reads the device off the staged break's \
             own line and the verdict counts `usb-storage: <bdf> slot <n> transport broke` for \
             that device alone, so the boot stick's clean recovery is no longer summed into the \
             injected disk's budget — and a line that stops naming a device is refused rather \
             than widened back to every disk on the machine",
        ),
        what: "the transport broke 3 times off one abandoned transfer, which can undo one \
               recovery and no more",
        evidence: "PR #41 (`wt/toyos-i8042tier`), run 31684437719, job 94397136494 \
                   (\"guest (4)\"), sha 711730204800d7173558f7dd96644c5910fb8cf0",
        source: "tests/common/usb.rs broke_on",
        measured: "2026-08-13",
    },
    Red {
        test: "std_unwind",
        instrument: Instrument::Ci,
        finding: Finding::fires(5, 5),
        standing: Standing::Disputed(
            "a since-deleted CI assessment said closed by `wt/toyos-fpu` \
             — `fxsave64`/`fxrstor64` on \
             all five Ring 3-reachable entries, with `fpu_isolation` as the gate that asks the \
             question on purpose. The write-up this row cites still counts it among the nine \
             that stand, and says the fix landed after this probe and has not been re-measured \
             on CI",
        ),
        what: "exit code Some(-1) — a #MF, vector 16, inside the unwinder on the spawned thread; \
               any Ring 3 process could leave a pending unmasked x87 exception behind and kill \
               the next unrelated process scheduled on that CPU",
        evidence: "probe-rate run 31258202923; isolated by probe-x87 run 31260763462, two arms \
                   of three reps differing only in `fault_gate_child`'s control word",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "std_unwind_so",
        instrument: Instrument::Ci,
        finding: Finding::fires(5, 5),
        standing: Standing::Disputed(
            "the same disagreement as `std_unwind`: the since-deleted CI assessment said \
             closed by `wt/toyos-fpu`, the cited write-up says not re-measured on CI",
        ),
        what: "the same #MF, on the same sub-test — the one that panics on a thread",
        evidence: "probe-rate run 31258202923; probe-x87 run 31260763462",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "metal_sim_null_audio",
        instrument: Instrument::Ci,
        finding: Finding::fires(5, 5),
        standing: Standing::Retired(
            "not soundd's device-less path, which was doing its job on every one of those boots: \
             the test read the line through a span of host wall clock. The null-sink probe, run \
             31263831141, three reps, caught it arriving 64 ms after a 500 ms window closed on one \
             rep and half a second before the ready marker on the other two. It waits on the guest \
             now",
        ),
        what: "soundd did not present a null sink on a device-less machine",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "hda_tone",
        instrument: Instrument::Ci,
        finding: Finding::fires(4, 5),
        standing: Standing::Stands,
        what: "1 mid-tone silence in the capture — gate A's harm verdict, which is not what #88's \
               `EXPECTED_FAILURES` entry covers: that entry names only \"the captured tone is not \
               one sine\"",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "late_storage_connect",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 5),
        standing: Standing::Retired(
            "what the actuator stages is an *ordering* — the disk arrives after the scan — so the \
             scan closes the window now and no host's boot speed can defeat it",
        ),
        what: "the boot scan bound a disk, so the port was not held empty",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "hda_two_live_refused",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 5),
        standing: Standing::Retired(
            "closed with `metal_sim_null_audio` and for the same reason: these two were the only \
             tests reading soundd's first line through a span of host wall clock",
        ),
        what: "\"presenting a null sink\" never reached the boot console",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "blocked_dump",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 5),
        standing: Standing::Stands,
        what: "two *different* reasons in the two reps — the census half, and /bin/terminal racing \
               the compositor",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "dump_nmi_probe",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 5),
        standing: Standing::Retired(
            "the actuator, not the guest's state: the deaf window spun on \
             `clock::nanos_since_boot`, whose 128-bit divide is an out-of-line call. It spins on \
             `rdtsc` against a `clock::tsc_deadline` now, so there is no address in the loop that \
             is not in `deaf_window` — 0 of 10 in run 31283095698",
        ),
        what: "the rip resolved to `u128_div_rem`, not to the spin",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "kernel_heartbeat",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 5),
        standing: Standing::Disputed(
            "two harness defects were fixed for it (the torn beat/pin pair, and a window that \
             opens at the first full mask), and the probe's fixed arm — run 31283095698, ten \
             reps — was **1 of 10 again**, on a *different* line: \
             `cpu6 last reached one 0.349s ago`. The \
             no-CPU-missing-from-two-consecutive-lines rule was written for that line and its rate \
             has not been re-measured",
        ),
        what: "2 of 12 heartbeats dropped a healthy CPU from the mask",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "usb_disk_index_stable",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 5),
        standing: Standing::Stands,
        what: "nothing enumerated on the first controller",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    // The twelve that came off the list when `wt/toyos-clock` landed. **These are
    // the rows the prose gets read backwards on.** They are measurements that the
    // name did not fire, on the same sixty jobs as the eleven above.
    Red {
        test: "metal_sim_client_death",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits, and the \"a guest stops making \
               progress and pays its whole ceiling\" shape went with it",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "metal_sim_window_drag",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "metal_sim_pointer_churn",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits. Read the later row for this name \
               before treating it as retired",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "metal_sim_compositor_stall",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "desktop_audio_client",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits. A thirteenth name with one sample \
               each way on one tree — stalled in run 31264914759 and passed in 31266194663, same \
               commit, half an hour apart — which is a rate and not a reproduction",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "desktop_typing_damage",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits. Distinct from the QEMU 8.2.2 red \
               measured earlier in the same campaign, which was closed by putting the dev host's \
               own QEMU in the container",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "doom_sound_flood",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "i8042_health_cadence",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "sshd_fail_closed",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "came off the list with `wt/toyos-clock`'s waits",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "xhci_hotplug",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "0 of 5, and the write-up says only that it *coincides* with `wt/toyos-clock`'s \
               waits — nothing named a trigger for the KVM wedge it used to give",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/xhci-flap-wedges-under-kvm.md",
        measured: "2026-08-08",
    },
    Red {
        test: "xhci_hid_break",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "0 of 5, and the write-up is explicit about what this does and does not cover: it \
               is about the \"guest stops making progress and pays its whole ceiling\" shape, \
               which prints a timeout. It is **not** cover for the endpoint-count red under the \
               same name",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "screen_pager_keys",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "0 of 5 on CI — while the dev host has it reproducing alone on `main` in the same \
               week, and `main`'s own CI went red on it once since. Read all three rows",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-08",
    },
    Red {
        test: "xhci_flap",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Stands,
        what: "PASS 5 of 5, in 7–9 s, on the same image and the same accelerator that wedged it — \
               and nothing in `toyos_xhci` changed between the two runs. A defect that stopped \
               appearing under an unchanged driver is a defect whose trigger nobody has named",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "issues/hardware/xhci-flap-wedges-under-kvm.md",
        measured: "2026-08-08",
    },
    Red {
        test: "xhci_slow_connect",
        instrument: Instrument::Ci,
        finding: Finding::quiet(5),
        standing: Standing::Retired(SLOW_CONNECT_CLEARS),
        what: "0 of 5 in the probe — which the write-up said was not the reassurance it looks \
               like, because the margin was inside the *guest's* boot and running alone moved it \
               by milliseconds rather than by a verdict",
        evidence: "probe-rate run 31258202923, tree f8f73e1, five reps",
        source: "tests/common/usb.rs xhci_slow_connect",
        measured: "2026-08-08",
    },
    Red {
        test: "xhci_slow_connect",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(SLOW_CONNECT_CLEARS),
        what: "`ALONE: red again — the defect is real`. `SLOW_CONNECT_NS` holds the ports empty for \
               0.3 s and the controller started at 0.296–0.311 s on a quiet host, so the gate red \
               whenever anything moved boot by ten milliseconds. That sensitivity is why the \
               log-ring regression was caught at all — no other gate in the suite noticed 350 ms — \
               and its own message names the fix: widen `SLOW_CONNECT_NS`, not the gate",
        evidence: "run 31261669826, the first on a tree carrying the harness's re-run-alone work",
        source: "tests/common/usb.rs xhci_slow_connect",
        measured: "2026-08-08",
    },
    // ---------------------------------------------------------------------
    // Run 31247206462: twelve shards on KVM at `--jobs 1`, 2026-08-08. Every one
    // of these was red again when re-run alone, and none reproduced on the dev
    // host. Recorded so that the next green run cannot quietly be read as their
    // absence.
    // ---------------------------------------------------------------------
    Red {
        test: "doom_sound_flood",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`timed out after 88s` alone, against 4–26 s on the dev host. Nothing here is \
               diagnosed, and it is 0 of 5 in the rate probe five days later",
        evidence: "run 31247206462, red again alone",
        source: "issues/audio/doom-audio-callback-stalled-on-the-t14.md",
        measured: "2026-08-08",
    },
    Red {
        test: "hda_client_stall",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "a DEADLOCK panic between the idle loop's log-file flush and the xHCI disk lock, in \
             the same run's own capture 24s before the wait gave up. The idle loop touches no \
             filesystem now, so this mechanism cannot recur",
        ),
        what: "`the ring arm: timed out`, and `timed out after 9s` alone",
        evidence: "run 31247206462, red again alone",
        source: "tests/common/hda.rs hda_client_stall",
        measured: "2026-08-08",
    },
    Red {
        test: "sshd_fail_closed",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "red alone in 22 s, having taken 152 s in the phase. Not diagnosed, and 0 of 5 in \
               the rate probe five days later",
        evidence: "run 31247206462, red again alone",
        source: "tests/toyos.rs sshd_fail_closed",
        measured: "2026-08-08",
    },
    Red {
        test: "xhci_hotplug",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`timed out after 66s` alone — `device_add`/`device_del` against a 100 ms debounce. \
               Green on the dev host in seconds and green under TCG on the same runner image and \
               the same QEMU",
        evidence: "run 31247206462, red again alone",
        source: "issues/hardware/xhci-flap-wedges-under-kvm.md",
        measured: "2026-08-08",
    },
    Red {
        test: "xhci_hid_break",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`timed out after 75s` alone — a staged transfer error on a HID endpoint. Green on \
               the dev host and under TCG on the same runner image",
        evidence: "run 31247206462, red again alone",
        source: "issues/hardware/xhci-flap-wedges-under-kvm.md",
        measured: "2026-08-08",
    },
    Red {
        test: "metal_sim_pointer_churn",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "closed — a console the test had counted before it caught up",
        ),
        what: "`bound 0 pointer sources` alone, over 8 plug/unplug cycles under a live compositor",
        evidence: "run 31247206462, red again alone",
        source: "issues/hardware/xhci-flap-wedges-under-kvm.md",
        measured: "2026-08-08",
    },
    Red {
        test: "xhci_flap",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`timed out after 164s` alone: three collapsed replugs survive and the fourth never \
               answers — the guest goes silent at about 4.4 s and never speaks again. Green under \
               TCG on the same runner image and the same QEMU, and green on the dev host, because \
               KVM runs the guest ~50× further between the host's two QMP writes",
        evidence: "run 31246245541, `debian:sid`/QEMU 11.0.3/KVM, `--jobs 1`, alone",
        source: "issues/hardware/xhci-flap-wedges-under-kvm.md",
        measured: "2026-08-08",
    },
    // ---------------------------------------------------------------------
    // `probe-green.yml` run 31282019974, tree 98e7247 (`main` 83ef8d1 plus the
    // workflow), 2026-08-09: ten reps, one job per rep and one `cargo test` per
    // name, aimed at the four names four consecutive red `main` runs had produced.
    // ---------------------------------------------------------------------
    Red {
        test: "desktop_window_child",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 10),
        standing: Standing::Stands,
        what: "the surface owner exited before it said it was ready — the /bin/terminal boot race, \
               2 of 10 **on a runner with one guest on it and nothing to contend with**. Rep 2 has \
               the compositor spawned at 0.347 s, the terminal at 0.349 s, and the terminal exiting \
               at 0.849 s one millisecond before the compositor maps its framebuffer",
        evidence: "probe-green run 31282019974, tree 98e7247, ten reps",
        source: "issues/kernel/desktop-window-child-freeze.md",
        measured: "2026-08-09",
    },
    Red {
        test: "dump_nmi_probe",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 10),
        standing: Standing::Retired(
            "the `rdtsc`/`clock::tsc_deadline` spin: 0 of 10 in run 31283095698",
        ),
        what: "`compiler_builtins::int::specialized_div_rem::u128_div_rem+0x99`",
        evidence: "probe-green run 31282019974, tree 98e7247, ten reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-09",
    },
    Red {
        test: "kernel_heartbeat",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 10),
        standing: Standing::Disputed(
            "run 31283095698, the fixed arm of the same ten reps, was 1 of 10 again on a different \
             line. See the note on this name's probe-rate row",
        ),
        what: "2 of 11 beats dropped a CPU from the mask",
        evidence: "probe-green run 31282019974, tree 98e7247, ten reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-09",
    },
    Red {
        test: "desktop_audio_client",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 10),
        standing: Standing::Retired(
            "soundd builds each line and issues one `write_all` (its local `say!`) now: 0 of 10 in \
             run 31283095698. The other 176 `eprintln!` sites in `userland/` still do not — and \
             the shape that left open is closed at the kernel by the log architecture: a \
             `ConsoleObject` per holder buffers its line and emits it whole under one \
             `BackendGuard`, so a kernel record cannot land inside one. `console_line_atomicity` \
             is the gate, 0 of 2000, and 8 of 8 red under `console-unbuffered`",
        ),
        what: "`STALLED` waiting for both clients to leave the mixer: `soundd: client ` and \
               `1 removed` came back either side of the kernel's four `exit:` accounting lines, so \
               the test counted one removal of two and waited out its 300 s guard. Systematic \
               rather than chance — soundd prints a client's removal exactly while the kernel \
               prints that client's exit",
        evidence: "probe-green run 31282019974 rep 10, and run 31271983043 on `main`",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-09",
    },
    // ---------------------------------------------------------------------
    // Run 31283095698, 2026-08-09: the fixed arm, ten reps of the same four names
    // on the same image with the same accelerator.
    // ---------------------------------------------------------------------
    Red {
        test: "dump_nmi_probe",
        instrument: Instrument::Ci,
        finding: Finding::quiet(10),
        standing: Standing::Stands,
        what: "0 of 10 against 2 of 10 in the arm before it",
        evidence: "probe-green fixed arm, run 31283095698, ten reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-09",
    },
    Red {
        test: "desktop_audio_client",
        instrument: Instrument::Ci,
        finding: Finding::quiet(10),
        standing: Standing::Stands,
        what: "0 of 10 against 1 of 10 in the arm before it",
        evidence: "probe-green fixed arm, run 31283095698, ten reps",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-09",
    },
    Red {
        test: "desktop_window_child",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 10),
        standing: Standing::Stands,
        what: "2 of 10, untouched by the three fixes that made the other names in the same arm \
               green. It is the one name left between `main` and the three-consecutive-greens \
               trigger",
        evidence: "probe-green fixed arm, run 31283095698, ten reps",
        source: "issues/kernel/desktop-window-child-freeze.md",
        measured: "2026-08-09",
    },
    // ---------------------------------------------------------------------
    // Single `ci` runs on `main`, each red once and adjudicated on its own.
    // ---------------------------------------------------------------------
    Red {
        test: "dump_nmi_probe",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "expiring the ask takes it back with a CAS, and a CAS that *fails* means the victim \
             went deaf on the boundary and the report is asked for after all — so the give-up was \
             repaired rather than the number tuned",
        ),
        what: "a signature neither probe produced — *the dump never ran*, both attempts. cpu0 waits \
               100 ms for the victim to reach its idle loop and on that runner it took 251 ms. \
               0 of 20 probe reps and 2 of 2 in one shard job",
        evidence: "run 31284962381, `main` at 1ed6f39",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-09",
    },
    Red {
        test: "late_storage_connect",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired("the scan closes the window now, not a duration"),
        what: "`xhci-slow-storage-connect` hid the disk's port for 300 ms — a claim about how far \
               into a boot `scan_ports` runs, true at 253 ms on the dev host and false at 407 ms \
               on that runner",
        evidence: "run 31286199802, `main` at 8d3f5b7",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-09",
    },
    Red {
        test: "screen_pager_keys",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(PAGER_ARITHMETIC),
        what: "keystroke 14 of 30. Bisected on the dev host to `f96d52e`, a merge whose two parents \
               are both green — and that bisect is the thing the retirement below is about",
        evidence: "run 31287853270, `main` at 53d29d5",
        source: "tests/toyos.rs screen_pager_keys",
        measured: "2026-08-09",
    },
    // ---------------------------------------------------------------------
    // `main`'s own `ci` runs, read off GitHub with `gh run view --log-failed` on
    // 2026-08-11. No write-up in the tree records these three, which is why they
    // are here: an index whose newest row predates the newest red is the thing
    // this file exists to stop.
    // ---------------------------------------------------------------------
    Red {
        test: "xhci_hid_break",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 15),
        standing: Standing::Retired(HID_SCOPED),
        what: "`3 endpoint(s) were found Running after the break, want 2` — dci 3 is the first IN \
               endpoint of every USB device, so one transport recovery on the boot USB disk \
               anywhere in the boot reds a test whose failure is about HID. `ALONE: GREEN` both \
               times, which the harness itself calls a rate and not a classification",
        evidence: "`main`'s fifteen most recent completed `ci` runs, 2026-08-09 to 2026-08-11: red \
                   in 31289459932 (a76a078) and 31331494794 (0e48d2e), read with \
                   `gh run view --log-failed`",
        source: "tests/common/usb.rs hid_broke_on",
        measured: "2026-08-11",
    },
    Red {
        test: "xhci_hid_break",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`STALLED: 133s of guard expired, and the guest had said nothing for the last 131s of \
               it` — the timeout shape the probe measured at 0 of 5, twice in one job, and \
               `ALONE: red again`. So that 0 of 5 is not cover for it either",
        evidence: "run 31422708833, `main` at 2572e4b, shard 10",
        source: "issues/hardware/eleven-names-red-on-ci.md",
        measured: "2026-08-10",
    },
    // ---------------------------------------------------------------------
    // The endpoint-count shape, seen on two PR branches rather than on `main`.
    // Neither branch's diff touches the test or the xHCI driver, so this is
    // the same defect the row above measures, on a denominator that row's
    // "fifteen most recent `main` runs" does not cover — hence `Seen`, not a
    // bump to that row's rate.
    // ---------------------------------------------------------------------
    Red {
        test: "xhci_hid_break",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(HID_SCOPED),
        what: "`3 endpoint(s) were found Running after the break, want 2` — the wide run's message. \
               The harness's re-run-alone failed too, but on a *different* assertion, the \
               `input never came back` shape `parallel-tests-red-under-other-suites.md` records on \
               the dev host — its first appearance on CI. `ALONE xhci_hid_break: red again` quoted \
               the wide run's message regardless, because that line always carries the original text",
        evidence: "PR #22 (`wt/toyos-endow`), run 31424496450 attempt 1, job 93586744461 \
                   (\"guest (5)\"), sha 73d0761b",
        source: "tests/common/usb.rs hid_broke_on",
        measured: "2026-08-10",
    },
    Red {
        test: "xhci_hid_break",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(HID_SCOPED),
        what: "`3 endpoint(s) were found Running after the break, want 2`, byte-identical between \
               the wide run (51s) and the alone re-run (9s) — the first occurrence where isolation \
               reproduces this exact assertion rather than going green or landing on the other \
               shape. 9s alone rules out host contention for this instance",
        evidence: "PR #35 (`codex/debug-wait-census`), run 31601325987, job 94129283847 \
                   (\"guest (5)\"), sha d522424e",
        source: "tests/common/usb.rs hid_broke_on",
        measured: "2026-08-12",
    },
    Red {
        test: "metal_sim_pointer_churn",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the capture was behind the colon the whole time, read 2026-08-22 with `gh run view \
             31396171916 --log-failed`: `[kernel 0.669 cpu1 tid=0] SEGFAULT tid=0: execute \
             unmapped address at 0x1b`, `cs=0x0008`, `rsp=0xffff800000dda000` with eight zero \
             quadwords from it, and one line later cpu0's `#PF UNHANDLED: cr2=0x0 rip=0x0 \
             err=0x10 user=false` — two CPUs dying at 0.669 s in the spawn burst, both on a \
             Ring 0 fetch at a tiny address. That is the signature whose four dev-host rows \
             were deleted with PR #149 (`SchedPass::answer_steal_requests`: a CPU handed a \
             thief the context it was still standing on — 13 deaths in 1,272 boots to 0 in \
             1,584), and the zeroed frame under it is what PR #202's backwards `memset` writes \
             (no Ring 0 entry cleared `DF`; 37 deaths in 13,960 twelve-wide boots without the \
             `cld`, 0 in 7,418 with it, p = 2.9e-9, commit 5e74971e). Both mechanisms are \
             gone, so the death this row measured cannot recur; a red under this name now is a \
             new measurement. The name was the workload, on a KVM shard, exactly as \
             `tests/CLAUDE.md` says",
        ),
        what: "`[qemu] Init process crashed during boot:`, 244 s in the phase, and \
               `ALONE: GREEN, and it was alone both times — nothing the harness controls differed, \
               so it failed once and passed once. That is a rate and not a classification`. The \
               name is 0 of 5 in the probe and declared closed in a write-up",
        evidence: "run 31396171916, `main` at 7af7c20, shard 2",
        source: "src/redlist.rs",
        measured: "2026-08-10",
    },
    // ---------------------------------------------------------------------
    // Invariant P on KVM. The dev-host row below predicted this case and said
    // what it would mean: "if invariant P ever fires on a KVM shard, this file
    // does not cover it". It has, twice. The two rows are the same assert on
    // two accelerators and they are not one measurement — magnitude separates
    // them, not call site: `driver::idle_loop` fired under both accelerators,
    // three orders of magnitude apart, so only the size of the overshoot says
    // which one ran.
    // ---------------------------------------------------------------------
    Red {
        test: "sched_check_build",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the assert is gone. Elapsed time across a pass is wall clock and a guest's wall \
             clock advances while the hypervisor has its vCPU, so the quantity carried a term \
             the kernel neither observes nor controls; `toyos-sched` records the distribution \
             and `tests/common/passcost.rs` judges it. **What retires this row is the panic, \
             which cannot happen again.** The replacement's first shape gated the 90th \
             percentile at `MAX_PASS_NS` on the argument that a busy host moves the maximum \
             and not the mass — an observed rate rather than a bound, and **measured false on \
             2026-08-18**: host load moves every order statistic, median as much as tail. So \
             the line is now the accelerator's own recorded sample, and for this instrument \
             that sample is sixteen CI runs and 7 612 passes with **zero over 200 000 ns**, \
             largest single pass 173 906 ns, 90th percentile 32 768 ns. The gate on this \
             instrument is therefore *tighter* than the number this row's assert stood over, \
             and a red under it is a fresh measurement rather than this one returning. A wider \
             survey taken the day before this retirement, never landed as its own commit, found \
             a second firing and a third sighting: run 31936533470, a push to `main`, `277260 \
             ns` on cpu1 in `driver::idle_loop` rather than `timer_handler`, 2026-08-16 08:28 \
             UTC — so the two KVM firings shared a call site with the TCG row below and only \
             magnitude told them apart. Of the 100 most recent `ci` runs through 2026-08-17 \
             13:29 UTC, 91 actually ran this shard and two fired, a rate of 2 of 91 that this \
             retirement supersedes rather than inherits: the assert those two firings are of \
             does not exist to fire a third time",
        ),
        what: "`invariant P: a scheduler pass took 200569 ns, budget 200000 ns` — the assert \
               firing on native x86-64 under KVM, in `timer_handler` -> `driver::pass` -> \
               `SchedPass::finish` while `test_rs_sched_stress` pid=7 was in syscall 8, at 1.449 s \
               of guest uptime. `schedule_no_return: panicked inside a pass, cannot rejoin` halts \
               every CPU 1 ms later, which is the whole of the 383 s of silence the run then \
               reported as `STALLED:`. **The red is the panic and the stall is its shadow**: \
               `run_test_paced` ends early on `KERNEL PANIC` only, which the CPU-exception path \
               prints and a Rust `panic!` does not, so the wait ran its full 382 s ceiling on a \
               machine that had been halted since second two and the summary said the run \
               `established nothing about this tree`",
        evidence: "PR #95 (`wt/toyos-harness2`), run 31946183485, job 95162423932 (\"guest (8)\"), \
                   sha 4ec5d01, on an Azure 4-core EPYC 7763 with `/dev/kvm` — so KVM nested in a \
                   hypervisor guest. One firing, not a rate: the earlier STALL under this name \
                   (run 31890991692, job 95027203184, guest 8, 2026-08-15) printed an empty \
                   `serial:` because `in_test` never became true, so its lines went to \
                   `TestResult::before` and the caller drops that — its cause is unrecorded and is \
                   not counted here",
        source: "tests/common/passcost.rs",
        measured: "2026-08-16",
    },
    // ---------------------------------------------------------------------
    // The dev host. Everything below is TCG on arm64, so none of it is evidence
    // about which vendor executes an instruction — and all of it is about a
    // machine CI has no way to construct.
    // ---------------------------------------------------------------------
    Red {
        test: "sched_check_build",
        instrument: Instrument::DevHostAlone,
        finding: Finding::fires(2, 2),
        standing: Standing::Retired(
            "the panic is gone and the machine survives, so what this row measured cannot \
             happen. The red under this name on this instrument is now the harness's \
             pass-cost gate refusing a distribution, which is the row below — a different \
             measurement of the same emulator, and the guest runs `sched_stress` to \
             completion under it",
        ),
        what: "`invariant P: a scheduler pass took 1684167 ns, budget 200000 ns`, panicking in \
               `driver::idle_loop` before userland — then 1749243 ns on cpu1 in the isolated \
               re-run. The dev host emulates x86-64 instruction by instruction while the guest \
               TSC advances with host wall clock, and eight to nine times the budget is what that \
               costs. Ruled out rather than assumed: removing `check_cpu` from inside the \
               measured window left 1705987 ns, and `pass` samples its clock after `drain_irqs`, \
               so the xHCI prologue is outside the window entirely",
        evidence: "`cargo test -- sched_check_build` on this branch, two boots (parallel phase \
                   then ALONE re-run); green on KVM the same day — twelve of twelve guest shards, \
                   run 31875856466, where it measured 5,879 ms. **What this row may no longer be \
                   read as saying is that the budget fits natively**: the same assert has since \
                   fired on KVM shards twice, at 277260 ns and 200569 ns, which is the \
                   `Instrument::Ci` row above. The TCG explanation of *this* magnitude stands — \
                   nothing on KVM has come within five times it — but the implied claim about the \
                   other accelerator does not",
        source: "tests/common/passcost.rs",
        measured: "2026-08-15",
    },
    Red {
        test: "sched_check_build",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(6, 10),
        standing: Standing::Retired(
            "the dev host takes no verdict on pass cost any more, so this red cannot be \
             produced. What retired it is the experiment this row's own last paragraph asked \
             for: 2026-08-18, six repetitions an arm, quiet and loaded interleaved in one \
             session, twelve CPU-runs each. **0 of 12 quiet CPU-runs over the budget at the \
             90th percentile against 9 of 12 loaded; 6 of 6 runs green against 6 of 6 red**, \
             with the arms separated by boot width 1.74x-2.34x against 2.66x-2.78x and \
             nothing else. The whole distribution translates one power-of-two bucket under \
             host load — median 65 536 -> 131 072 ns, p90 131 072 -> 262 144 ns — and 200 000 \
             sits between the two, which is the entirety of why the verdict flipped. So \
             `tests/common/passcost.rs` records that sample and, because it spans four buckets \
             on one unchanged tree, has cross-arch TCG report its distribution and judge no \
             magnitude at all. `sched_check_build` still gates the clean boot, the three \
             check-build asserts and `sched_stress` on this instrument; only the cost half is \
             now CI's alone",
        ),
        what: "`this distribution has mass over the budget: nine passes in ten must be provably \
               under 200000 ns and it cannot show that` — the harness's pass-cost gate, which \
               replaced the panic, refusing a distribution the *host* inflated. The guest is \
               fine either way: `sched_stress` runs to completion and prints `all sched_stress \
               tests passed`. **Contention moves this guest's median by a factor of eight and \
               the gate follows it**: with the machine to itself, `cpu0: 168 passes, p50 < \
               16384 ns, p90 < 131072 ns, max 1504209 ns, 7 over the 200000 ns budget` and it \
               passes; in the same suite's 12-wide phase, `cpu0: 134 passes, p50 < 131072 ns, \
               p90 < 262144 ns, max 1745977 ns, 14 over` and it reds. **Note the maxima on the \
               green side**: 1974235 ns and 2543303 ns in the serial tail of the run that ended \
               263 of 263 green — nine and twelve times the budget, refused by nothing, where \
               the assert this replaced would have halted the machine on any one of them. \
               Every red measured carried `p50 < 131072 ns` against `p50 < 16384` or `< 32768 \
               ns` on every green — the median moving with the 90th percentile, which is the \
               observation the controlled experiment then confirmed and which is why no line \
               over this distribution survives on this instrument",
        evidence: "`cargo test` on `wt/toyos-invariantp`, 2026-08-17, ten CPU-runs over three \
                   sessions. Every one of the six reds was taken beside other guests: four \
                   under another agent's suite on the shared host (`fastest boot 2330 ms \
                   against the reference 1320 ms`, 1.77x) and two in a 12-wide parallel phase. \
                   Every one of the four greens had the machine to itself — an isolated re-run \
                   at 1.02x and the serial tail at 1.05x. `sched_check_build` is `Sched::Serial` \
                   since the same day, on this measurement, which removes the second half of \
                   this row's own cause. Not evidence about KVM in either direction — the dev \
                   host boots no KVM guest. **This row is also the counter-evidence to the \
                   gate's own warrant**: 14 of 134 and 19 of 140 passes over budget is 10–13 %, \
                   which is correlated inflation with mass in it rather than the handful of \
                   samples the gate's argument assumed a busy machine produces. The controlled \
                   experiment that turned that counter-evidence into the retirement above is in \
                   `tests/common/passcost.rs`",
        source: "tests/common/passcost.rs",
        measured: "2026-08-17",
    },
    Red {
        test: "screen_pager_keys",
        instrument: Instrument::DevHostAlone,
        finding: Finding::fires(3, 3),
        standing: Standing::Retired(PAGER_ARITHMETIC),
        what: "`0 page moves over 30 keystrokes in 0.4s — an unattended deadline alone could have \
               produced 1.1 of them`. Not load: the landing gate that produced one of them ran at \
               1.05× the reference boot and the failure was byte-identical to the ones taken at \
               load 11–16. Bisected to `f96d52e`, a merge whose two parents are both green",
        evidence: "`main` at b36cf64, three runs alone in one session; seven boots across the bisect",
        source: "tests/toyos.rs screen_pager_keys",
        measured: "2026-08-08",
    },
    Red {
        test: "hda_tone",
        instrument: Instrument::DevHostAlone,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`1 mid-tone silences in the capture: total 1 [1p×1]` — the harm assertion, which \
               #88's exemption is right not to cover, so any landing whose gate is `cargo test` is \
               red on `main` for this and an agent will read it as theirs",
        evidence: "`main` at 6d11938, alone",
        source: "issues/audio/hda-tone-red-beyond-its-exemption.md",
        measured: "2026-08-07",
    },
    Red {
        test: "hda_tone",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 3),
        standing: Standing::Retired(
            "the splice is unrepresentable in the log architecture: a `ConsoleObject` per \
             holder buffers its line and emits it whole under one `BackendGuard`, so the kernel \
             record that cut this needle open has nowhere to be acquired. `console_line_atomicity` \
             is the gate, 0 of 2000 with 8 of 8 red under `console-unbuffered` \
             (2026-08-15, at counts from 1 to 570 of 2000 — the magnitude is a race and only \
             the sign is a verdict)",
        ),
        what: "the needle `soundd: hda codec0 vendor=1af4` split in half by another writer, between \
               `codec` and `0`. Three full suites on one tree in one session, red on the third — so \
               it is not the audio path and not load in any way a re-run answers; it is which two \
               writers happen to collide",
        evidence: "landing-1786130703-71774.log, a documentation-only branch",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    Red {
        test: "hda_tone",
        instrument: Instrument::DevHostAlone,
        finding: Finding::quiet(3),
        standing: Standing::Retired(
            "the loaded arm it was the control for is retired above; a quiet reading whose red \
             half has gone is not evidence of anything on its own",
        ),
        what: "green 3 of 3 alone on a quiet host, against the splice red in the same session",
        evidence: "the same session as the splice above",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    Red {
        test: "audio_tone_load",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "gate A's fast tier, failing its own two-boot rule — dropouts on the first boot *and* \
               on the confirming re-boot — four times in one session at smp=1, on two different \
               trees. **The denominator is not readable**: the closed sighting file said \"six \
               runs in one session\" while its own listing is four reds, one green and \"twice \
               more GREEN\", which is seven; its tables live in the commit that closed it into \
               the source entry. smp=8 failed the same rule twice on 2026-08-07",
        evidence: "2026-08-04 session, 5408cfb with the bundle stashed and bundle D alternating",
        source: "issues/audio/thorough-tier-reds-on-unmodified-main.md",
        measured: "2026-08-04",
    },
    Red {
        test: "audio_tone_load",
        instrument: Instrument::DevHostAlone,
        finding: Finding::quiet(3),
        standing: Standing::Stands,
        what: "`main`, alone, three times, green at both widths, with wake latencies of 6.5–54 ms \
               where every red carried 76–297 ms — soundd not being scheduled rather than a cost \
               per period",
        evidence: "task #58's A/B session, `main`'s tip against a branch, one host",
        source: "issues/audio/thorough-tier-reds-on-unmodified-main.md",
        measured: "2026-08-07",
    },
    // The contention class. Every one of these is a verdict that expires on the
    // host's clock, and CI is structurally unable to produce or refute one.
    Red {
        test: "i8042_mouse",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`1003 pointer events reached userland out of 1004 packets injected, never more than \
               4 of them (12 bytes) outstanding against a 16-byte device queue` — inside the bound \
               the summing fix installed, so that mechanism is not what this is. A/B in one session \
               put `main`'s kernel red with the identical line and the branch green",
        evidence: "two full suites in one worktree while a second held six of the twelve guest slots",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    Red {
        test: "i8042_absent",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`601ms without an i8042 and 287ms with one` against a 300 ms allowance. The absolute \
               figure moved 277→619 ms across three runs of one boot with no code change, and it is \
               already `Sched::Serial`, so intra-suite width is not what reaches it",
        evidence: "a landing gate, then alone minutes later on both trees",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-04",
    },
    Red {
        test: "desktop_locale_detect",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`nothing typed at the terminal window reached a shell`, `ALONE … GREEN`, on a branch \
               that touches neither the compositor nor the terminal",
        evidence: "one full suite on a host carrying three to four concurrent suites",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-05",
    },
    Red {
        test: "netd_connection_caps",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "red at 50 s inside a landing gate that was otherwise 257/259, green in 7 s alone on \
               the same tree moments later, on a branch that touches neither netd nor the network \
               stack",
        evidence: "a landing gate, then alone on the same tree",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-05",
    },
    Red {
        test: "dump_nmi_probe",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`the NMI went unanswered too` — its wall-clock verdict expiring on a host carrying \
               three other worktrees' suites. It is `Sched::Serial`, so it failed in the serial \
               tail and the harness never re-ran it alone; run alone moments later it passes in \
               23 s. Nothing should widen its millisecond",
        evidence: "one full suite; the run's `[host-slots]` lines name all three worktrees",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    Red {
        test: "blocked_dump",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`nothing typed at the terminal window reached a shell`, `ALONE … GREEN` in 5 s. Its \
               verdict is the dump's content, but *reaching* the dump crosses a compositor, a \
               terminal and a shell, and that step is a wall-clock margin",
        evidence: "one full suite under load, and a second landing gate in the eight-landing regime",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    // ---------------------------------------------------------------------
    // **`fd_lifetime` is `handle_lifetime` since 2026-08-20.** The rename is the
    // fd/inbox naming wave's — owner ruling of 2026-08-19, "fds belong only in
    // libc jargon", which `kernel/src/object/handle.rs` states where it binds.
    // Every reading in the three rows below was taken before it, so the harness
    // lines they quote and the command they name printed and spelled
    // `fd_lifetime` at the time; all three have been re-spelled to the live
    // name, because a row naming a test that no longer exists matches nothing,
    // adjudicates nothing, and hands whoever re-runs it a command that fails.
    // ---------------------------------------------------------------------
    Red {
        test: "handle_lifetime",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(4, 7),
        standing: Standing::Retired(
            "the reading was early, not polluted — 2026-08-19, and both halves of this row's \
             argument are withdrawn. CI run 32237424649 put it red on `Instrument::Ci`, where \
             there is one guest per machine, and the harness's `ALONE` re-run — a fresh boot \
             carrying that binary and nothing else — was red again on the same failure. So \
             `ALONE … GREEN every time` is false and the neighbours are not what this is. \
             Twenty kill rounds alone in the guest at `8e9f851` say what it is: in ten of them \
             the deficit after `wait` decays **two megabytes at a time** across eight \
             back-to-back `SYS_SYSINFO` calls — `[12, 10, 10, 10, 8, 6, 4, 2]` MiB — and free \
             memory returns to its baseline every single round, drift zero. Nothing leaks; the \
             test reads before the release has run. `object::drain_zero_handles` clears \
             `ZERO_PENDING` before it runs a hook, so a batch another CPU took is \
             indistinguishable from an empty queue and the killing syscall returns with its \
             objects unreleased — caught in a kernel trace as eight `RingRef` frees landing \
             after `kill_process` returned, and as a second CPU taking a batch mid-kill. Both \
             binaries now settle before reading, and both read the per-kind object census \
             rather than the machine's free memory; the kernel half is \
             `issues/kernel/deferred-release-outlives-its-syscall.md`. \
             **`Sched::Serial` would have retired nothing**, which is why that proposal is \
             recorded as ruled out rather than left standing",
        ),
        what: "`a killed process kept 16777216 bytes of its io_urings`, `ALONE … GREEN` every \
               time. `kill_releases_ring` asks `SYS_SYSINFO` for the **machine's** free memory \
               either side of a kill, and it shares the `tests/testcases` boot with every other \
               Rust guest binary — so the verdict is only sound while nothing else in that guest \
               holds or releases a page across the window, which nothing arranges. `/bin/logd` \
               joining every image is what made it loud: it holds an `io_uring`, a 64 KiB record \
               buffer and a `File` whose page-cache pages come and go",
        evidence: "a same-session A/B of two seven-suite arms, 12 wide, on one dev host: 0 of 7 at \
                   a76ffd0 against 4 of 7 at 19ce5d0, whose diff is comment text, one \
                   caller-less kernel function deleted and a test-runner gate nothing on this \
                   boot invokes. Two earlier sevens on the same two trees gave 1 of 7 and 2 of 7, \
                   so the rate this row carries is the widest of four readings and not the only \
                   one",
        source: "issues/kernel/deferred-release-outlives-its-syscall.md",
        measured: "2026-08-15",
    },
    // ---------------------------------------------------------------------
    // `wt/toyos-fdleak`, 2026-08-19, at `8e9f851`. The run that broke the row
    // above it out of its explanation, and the two dev-host readings taken
    // against it. Kept as three rows because they are three measurements on
    // three instruments and no one of them says what the others do.
    // ---------------------------------------------------------------------
    Red {
        test: "handle_lifetime",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the settle landed: both free-memory verdicts read once the machine has stopped \
             giving memory back — samples 10 ms apart until two agree, bounded at a hundred, \
             the last reading handed back either way. A liveness bound and not a margin, so a \
             kernel that frees nothing is quiescent on the first pair and reds at once",
        ),
        what: "`a killed process kept 16777216 bytes of its io_urings` — the **whole** 16 MiB \
               the holder allocated, against a 6 MiB threshold, so the release had made no \
               progress at all when the reading was taken. `ALONE handle_lifetime: red again, the \
               same failure both times`, which on a shared-block name is a fresh boot carrying \
               that binary and nothing else",
        // The `gate A, thorough` half of this citation was struck 2026-08-21.
        // That workflow ended its step in `exit "${PIPESTATUS[0]}"` under a shell
        // with no `PIPESTATUS`, so it reported `failure` on every run it ever had
        // whatever the audio said — and on 2026-08-19 both of its shards printed
        // `[gate A] PASS`. It was never evidence of anything about `main`. The
        // `ci` half is a verdict, and it is what this row rests on.
        evidence: "CI run 32237424649 (PR #126, job `guest (1)`); `main` red at `8e9f851` on \
                   `ci` the same day",
        source: "issues/kernel/deferred-release-outlives-its-syscall.md",
        measured: "2026-08-19",
    },
    Red {
        test: "handle_lifetime",
        instrument: Instrument::DevHostAlone,
        finding: Finding::quiet(20),
        standing: Standing::Retired(
            "the instrument it measured is gone. `kill_releases_ring`'s verdict is now the \
             kernel's live `Inbox` count either side of the kill, not the machine's free memory, \
             so neither the neighbours nor the margin this row is about exists any more: a leak \
             of one ring is `+1` where 2 MiB fitted inside 6 MiB and passed. Measured with that \
             leak deliberately planted, 2026-09-01: green on the free-memory verdict, red on the \
             census, both arms on this host",
        ),
        what: "twenty consecutive filtered runs of the unmodified binary, all green — which is \
               why every dev-host sighting of this name had said `ALONE … GREEN` and why the \
               defect was read as its neighbours' page churn. The dev host is two CPUs under \
               TCG; CI is four under KVM, and the race widens with the CPU count",
        evidence: "20 × `cargo test --test toyos-build -- handle_lifetime` on one quiet dev host, \
                   `wt/toyos-fdleak` at `8e9f851`",
        source: "issues/kernel/deferred-release-outlives-its-syscall.md",
        measured: "2026-08-19",
    },
    // `shm_release_reclaims` gets no row: it has the same instrument with the
    // same hole and it took the same settle, but nothing here measured it red,
    // and a `Seen` written from an argument rather than a run is exactly the
    // overstatement this index refuses. It took the same census with it.
    Red {
        test: "fs_dirs_durable",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`the staged directories left the log volume breaking the format`, and it says \
               four things at once — an unmirrored FAT entry, a chain one cluster longer than \
               `DIR_FileSize`, two clusters allocated and unreachable, and `FSI_Free_Count` \
               three off. Four format complaints on one volume is the signature of a read that \
               beat its writer, not of four faults. `ALONE … GREEN`, so the harness classifies \
               it as `Sched::Parallel` and the run stays red on that. The fourth name of the \
               shape `fat_backing_revoked`, `device_claim_lifetime` and `esp_filesystem` \
               already carry",
        evidence: "one full `cargo test` an arm, back to back on this dev host 2026-09-01: \
                   `w5b5-host-build` at dbc7d610 (this red, host at 1.23x width) against \
                   627e5f0f (no red under this name; one `i8042_undecoded_bytes`, also \
                   `ALONE … GREEN`, host at 2.66x width). One run an arm is a sighting and not \
                   a rate",
        source: "issues/build/a-loaded-suite-reds-a-volume-checker-on-both-arms.md",
        measured: "2026-09-01",
    },
    Red {
        test: "screen_console_scroll",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`round 1: the guest never printed CHURN-DONE 0 100`, **598 s** in the wide phase \
               against a phase that is ~45 s on a quiet host, `ALONE … GREEN`. The landing gate it \
               killed ran 778.9 s with four other `--land` processes on the host, on a branch whose \
               whole delta was two documentation lines. **2026-08-22:** a kernel death of PR \
               #202's class (no Ring 0 entry cleared `DF`; 37 deaths in 13,960 loaded boots \
               before the `cld`, 0 in 7,418 after) leaves exactly this capture too — a guest \
               that stops mid-test under load, on a date before any wait could see a panic — \
               and nothing in it separates that from the host. One sighting, no denominator \
               on record; what retires it is loaded suites of the fixed tree with no red under \
               this name, three by the Poisson rule (p = e^-3 against a rate of one per suite). \
               That count is owed: the 2026-08-22 sweep found the guest suite refused in its \
               worktree for the whole session, the shared sysroot being claimed by \
               `wt/toyos-census`'s ABI landing (#209)",
        evidence: "a landing gate on a documentation-only branch",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    Red {
        test: "xhci_hid_break",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`input never came back: no pointer event moved by (2560, -1920); deltas seen: \
               [(256, 256), (256, 256)]`, `ALONE … GREEN`. The two deltas it did see are the \
               boot-time absolute tablet, so what went missing is the relative mouse's event after \
               the staged break — a wall-clock margin on the recovery path, not a recovery that \
               failed",
        evidence: "a landing gate on a branch whose delta was one documentation commit",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    Red {
        test: "screen_early_panic",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`ALONE … GREEN`. One branch's two consecutive landing gates died on two *different* \
               tests from this list — `blocked_dump`, then this — with eight `toyos-build --land` \
               processes queued on the integration lock at once. Guest slots bound guests, and a \
               landing storm is not made of guests",
        evidence: "the eight-landing regime, 2026-08-07",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    Red {
        test: "screen_blocked_dump",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the defect was in the kernel and closed 2026-08-08 — which is the caution the rest of \
             that list now carries: it reds at ~20% with the host to itself, so `ALONE: GREEN` on \
             it said \"this re-run was one of the four green ones\" and not \"the phase did it\"",
        ),
        what: "`ALONE: GREEN` twice and `ALONE: red again` once across four full suites in one \
               session",
        evidence: "four full suites on `wt/toyos-tlbfix`, 2026-08-07",
        source: "issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md",
        measured: "2026-08-07",
    },
    Red {
        test: "screen_blocked_dump",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 2),
        standing: Standing::Stands,
        what: "`the report the keystroke painted does not carry \"== VERDICT:\"`; the decoded \
               panel was the boot-log tail ending `[page 2/4]`, with none of the dump's three \
               summary markers. The isolated re-run painted `0 overdue, 0 absurd, 0 unheld, 0 \
               never ran` and passed in 6 s. **2026-08-22:** the dump is dispatched from a \
               scheduler pass, so a machine whose CPUs had halted paints exactly this panel — \
               the boot-log tail and no verdict — and a kernel death of PR #202's class (no \
               Ring 0 entry cleared `DF`; 37 deaths in 13,960 loaded boots before the `cld`, \
               0 in 7,418 after, and the class has one KVM sighting) is a cause this capture \
               cannot exclude. Not shown: no serial line names a death, and the sighting \
               predates the wait that would have. Retires on the hosted lane by the Poisson \
               rule: at the recorded rate of one red in two runs, six consecutive green runs \
               (p = e^-3)",
        evidence: "PR #33 run 31472702284, job 93736011023, merge ref \
                   1d19104d1b832da1aaad43906e0673cb87db93ba",
        source: "issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md",
        measured: "2026-08-11",
    },
    Red {
        test: "screen_blocked_dump",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`the report the keystroke painted does not carry \"== VERDICT:\"`, after 520 s in \
               the wide phase; the isolated re-run was green. This is the no-verdict shape, not \
               the retired compositor-overlay red under the same test name. **2026-08-22:** as \
               the CI row under this name — a machine whose CPUs halted paints this panel, so a \
               kernel death of PR #202's class (37 deaths in 13,960 loaded boots before the \
               `cld`, 0 in 7,418 after) is a cause the capture cannot exclude and does not \
               show; twelve wide beside a second worktree's suite is that class's exposure. One \
               sighting; retires at three loaded suites of the fixed tree with no red under this \
               name (p = e^-3 against one per suite). That count is owed: on 2026-08-22 the \
               guest suite was refused behind `wt/toyos-census`'s sysroot claim (#209)",
        evidence: "one 12-wide full suite on 2026-08-09 while a second worktree's suite was live, \
                   then the harness's isolated re-run",
        source: "issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md",
        measured: "2026-08-09",
    },
    Red {
        test: "desktop_typing_damage",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(3, 7),
        standing: Standing::Retired(
            "`shell_answers` retyped `echo <nonce>` against `qemu::budget(20 s)` because nothing \
             knew when the terminal was up, so \"how long does a desktop take to come up on the \
             host of the day\" *was* the verdict. The terminal prints `terminal: ready` now and the \
             coming-up half waits on the guest's own liveness",
        ),
        what: "`nothing typed at the terminal window reached a shell`, 243–255 s in the wide phase \
               against 16 s alone. The victim is positional: `desktop_window_child` held a lane for \
               ~250 s of every run and whichever desktop the duration profile ranked next went in \
               beside it",
        evidence: "seven full runs in one worktree, one session",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-06",
    },
    Red {
        test: "desktop_audio_client",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 7),
        standing: Standing::Retired("`terminal: ready`, as for `desktop_typing_damage`"),
        what: "248 s in the wide phase against 14 s alone — the same lane, promoted into it by the \
               duration profile. It cost another landing on 2026-08-07 at 787 s wide against 14 s \
               alone, with its own verdict line rather than the typing one, so the message is not \
               the tell and the pair of durations is",
        evidence: "seven full runs in one worktree, one session",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-06",
    },
    Red {
        test: "desktop_window_child",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(10, 10),
        standing: Standing::Retired(
            "not a guest defect: `close_focused_window` looped on `log[new..]` but waited with \
             `serial_until`, which scans the whole capture, so the previous probe's `windows=1` \
             answered instantly and it re-sent GUI+Q at the speed of a QMP round trip — closing the \
             window under the one it meant. It waits on the compositor's `note_closed` event now. \
             **This retires the test's reds and not #156**, whose signature is a guest that goes \
             silent",
        ),
        what: "hit 10/10 across four invocations in the 12-wide parallel phase, with the harness's \
               re-run-alone pass reporting GREEN each time",
        evidence: "four invocations by an agent landing unrelated documentation",
        source: "issues/kernel/desktop-window-child-freeze.md",
        measured: "2026-08-06",
    },
    Red {
        test: "metal_sim_window_caps",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "two CPUs shooting down at once — a mutual wait and not a bound, so no deadline value \
             was ever going to fix it. `kernel/src/shootdown.rs`, gated by \
             `an_initiator_answers_while_it_waits`",
        ),
        what: "FAIL 5 s in the wide phase three times, PASS 3 s alone on the branch and 36 s alone \
               on `main`. Its own work *completes* — `window caps: oversized refused, 62 windows \
               granted then refused` — and the process then exits `-1` after two CPUs have each \
               stalled five seconds on `tlb: cpu N has not flushed for generation …`",
        evidence: "two `--land` gates on `wt/toyos-boot` and five A/B runs against `main` at \
                   6d11938, one session",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    Red {
        test: "null_sink_shipped_client",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Retired("the same shootdown deadlock"),
        what: "FAIL 10 s in the wide phase, PASS 4 s alone on the branch and 5 s alone on `main`, \
               with the same two `tlb:` lines in the capture",
        evidence: "two `--land` gates on `wt/toyos-boot`, one session",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-07",
    },
    // ---------------------------------------------------------------------
    // `wt/toyos-logd`, dev host, 2026-08-15: **fourteen** full suites in one
    // session — an interleaved A/B of L3's review finding F1, five suites with
    // the branch tip's single `BackendGuard` acquisition around a
    // userland-chosen length and nine with `write_console`'s window bounded
    // (five of the A/B and the four the landing gate then ran). **That the
    // rates do not move is the finding**: the branch's remaining suspicion for
    // both names was an interrupts-off window it owns, and bounding the last
    // one leaves each where it was — i8042 2 of 9 against 1 of 5, macro 3 of 9
    // against 1 of 5, which for counts this size is the same rate. So what is
    // left belongs to the two write-ups cited here. Adjudicated rather than
    // carried, per the root CLAUDE.md.
    // ---------------------------------------------------------------------
    Red {
        test: "i8042_undecoded_bytes",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(3, 14),
        standing: Standing::Retired(
            "the counters are one word, 2026-08-17 — and this retirement is a measurement where \
             the one it replaces was an argument. `kernel/src/drivers/i8042/tally.rs` is a single \
             `u64` the ISR writes **once, after the burst**, low half the interrupts that put a \
             byte in the ring and high half those that found none, and `Counts` can only be built \
             by one load: there is no subtraction left to be wrong and no instant at which the \
             halves disagree. Moving that write to the end also ends the producer this row's own \
             line actually had — `IRQS` moved on the way *in*, so a reader between the pin and the \
             first `push_isr` held a count of arrived bytes with no byte anywhere — and `RX_BYTES` \
             is now counted in `pop` rather than after the drain, so a byte in mid-decode is on \
             one side of the report's `has_bytes` guard rather than neither. Between them `N \
             interrupts and 0 bytes, nothing decoded` is unprintable. **Measured in both \
             directions**: `kernel-loom/tests/i8042_tally.rs` reds with the two counters put back \
             (`Counts { carried: 1, empty: 0 }` for an interrupt that carried nothing) and passes \
             with the word, and a third model asserts the old shape really is read torn so the \
             file cannot pass vacuously. **Nine full `cargo test` suites, 9 green.** Not cover for \
             the CI row under this name, which is a different producer and still stands",
        ),
        what: "`the line names no byte: [kernel 0.418 cpu1] i8042: 1 interrupts and 0 bytes, \
               nothing decoded — first seen at 418ms`. The test takes the *first* `nothing \
               decoded` line in the capture and assumes it is the one its injection produced, and \
               an interrupt whose byte the driver's own polling init already consumed produces an \
               earlier one. **The isolated re-run answered differently on the two arms** — `red \
               again` on one occurrence and `ALONE: GREEN` on the other — which is itself evidence \
               that the timing and not the arm decides it. \
               \n\n**Retired 2026-08-16, and the retirement is withdrawn 2026-08-17: the driver \
               half does not hold.** It claimed the driver says `nothing decoded` only when \
               something arrived to decode. It does not, because the two counters that decide are \
               read torn: the ISR adds to `IRQS` on entry (`i8042/mod.rs:663`) and to `EMPTY_IRQS` \
               only after draining and finding nothing (`:693`), with the port-drain loop between \
               them, while `report_health` computes `carried = IRQS - EMPTY_IRQS` (`:390`) and \
               prints whenever `carried > 0`. A reader landing inside that window sees \
               `carried = 1` for an interrupt that carried nothing. Observed \
               again 2 of 6 full suites on 2026-08-17 (`1 interrupts and 0 bytes … first seen at \
               449ms`, PR #106's author, on a tree containing the fix). Whether the test half — \
               anchoring on `===I8042_READY===` — holds is a separate question and is not decided \
               here. \
               \n\n**The withdrawal above went on to say the torn read `prints this row's line \
               exactly`, and that clause was wrong** (corrected 2026-08-17, PR #114). The torn \
               read is real and is fixed, but it is not what produced this line, and the boot \
               order says so: the reporting CPU is `cpu1`, an AP, and `i8042::init` runs on the \
               BSP *before* `smp::boot_aps` — so at the bring-up interrupt this row always named \
               there is no second CPU in existence to land inside the ISR's window. What did \
               produce it is a different window in the same handler: `IRQS` was incremented on \
               **entry**, ahead of the first `push_isr`, so a reader between the pin asserting and \
               the first byte reaching the ring held a count of arrived bytes with no byte \
               anywhere — `carried = 1`, `RX_BYTES = 0`, `has_bytes()` false, which is this line. \
               Both windows close the same way and did, in PR #111. **The withdrawal itself \
               stands**: retiring on reasoning alone was wrong whichever mechanism the reasoning \
               named, and that is the half worth keeping",
        evidence: "fourteen full `cargo test` suites in one session on `wt/toyos-logd`: 2 of the 9 \
                   with the window bounded and 1 of the 5 without; `main` (4d8c2e9) 0 of 7 and \
                   this branch 0 of 5 before the byte ring went, both recorded in the tracker \
                   entry this row was filed against, closed and kept by git history",
        source: "kernel-loom/tests/i8042_tally.rs",
        measured: "2026-08-15",
    },
    Red {
        test: "71_macro_empty_arg",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(4, 14),
        standing: Standing::Retired(
            "the capture path stopped attributing other processes' bytes to this program, \
             2026-08-15. Two causes, both measured rather than argued: a daemon's whole line \
             landing in the window, which `common::console::verdict` removes on the boot \
             config's own list of who may speak; and the one no rule over whole lines reaches \
             — this case is `printf(\"%d\", …)` with no newline, so its `17` reaches the wire \
             unterminated and the host's splitter appends whoever wrote next, giving \
             `17init: started test-runner` in one line, or `17===TEST_END …` and an empty \
             capture. After the fix: 10 targeted runs and 5 full suites, 0 reds, against 1 of \
             10 with only the first cause answered. `c_capture_ignores_daemon_lines` is the \
             gate and carries both captures verbatim",
        ),
        what: "`output mismatch`, expected `17` and the capture empty — the child's own line fell \
               outside the `===TEST_START===`/`===TEST_END===` window the C family compares whole. \
               Same shape and same test name the write-up records at `dbbdcbe`, which is before \
               this branch existed. **A five-run sample once read as bounding the console drain \
               taking this to zero; fourteen suites here say roughly one in four whatever the \
               console lock does**, so that was a lucky five rather than a fix. The console lock \
               was never in it: what decided the rate was \
               whether the writer after this program was the kernel, whose `[kernel ` prefix the \
               capture already cut at",
        evidence: "the same fourteen suites as the row above: 3 of the 9 with the window bounded \
                   and 1 of the 5 without",
        source: "tests/common/console.rs",
        measured: "2026-08-15",
    },
    // ---------------------------------------------------------------------
    // The same session's landing gate, and a second measurement of one of the
    // names above rather than more of the first: ten full suites back to back
    // with no gap, where the fourteen were spaced. It is a different
    // instrument in everything but the label, and the rate says so.
    // ---------------------------------------------------------------------
    Red {
        test: "i8042_undecoded_bytes",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(6, 10),
        standing: Standing::Retired(
            "the same one word as the row above, 2026-08-17, and this rate is the one the nine \
             green suites were run against: back to back with no gap, on a host another \
             worktree's suite was taking guest slots from, and **two of the nine collapsed \
             machine-wide** — 160 and 172 reds on `Broken pipe` and `QEMU disconnected` — with \
             this name passing inside both. That is the load this row says the rate tracks, at \
             more of it than the row was measured under, with nothing to track",
        ),
        what: "the same line, and **the rate tracks host load** — 6 of 10 with the suites run back \
               to back and the load average never below 6.4, against the 3 of 14 above with the \
               host allowed to settle between them, on one tree in one session. The harness's \
               isolated re-run answered `ALONE: GREEN` on these, which is the class name for \
               exactly that. A bring-up race whose window is the driver's own polling init is what \
               a rate that moves with the host looks like; a defect in what this branch changed is \
               not. **Retired with the row above on 2026-08-16 and withdrawn with it on \
               2026-08-17** — a rate cannot be retired by a fix that does not reach its cause. \
               The withdrawal named the torn read as that cause and **that attribution was wrong** \
               (corrected 2026-08-17, PR #114): this is a rate of the entry-time increment, not of \
               the subtraction, for the reason the row above sets out — no AP exists to read \
               anything at the bring-up interrupt. The withdrawal was still right to be made, \
               which is the durable half of it",
        evidence: "ten consecutive full `cargo test` suites on `wt/toyos-logd`'s tip, loads \
                   6.4-9.7, immediately after the fourteen above",
        source: "kernel-loom/tests/i8042_tally.rs",
        measured: "2026-08-15",
    },
    Red {
        test: "boot_partition_identity",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 10),
        standing: Standing::Retired(
            "the producer was read out of the code and fixed, 2026-08-22: `toyos::net::hangup` \
             covered only `IpcError::Disconnected`, which `ipc::read_exact` raises on a read that \
             answered zero — so it reached the netd that left while this endpoint was waiting for \
             the *response*, and neither of the two writes ahead of it. Both now map to \
             `NetError::NetdNotFound`, which is `ErrorKind::NotConnected`, which is sshd's quiet \
             arm. Landed `f12b684f`, PR #217, with the guest sequence `netd_gone_mid_bind` \
             (PR #218) staging all three refusals against a port of its own and no timing at all. \
             Retired 2026-08-23; the rate's cause is gone, so the rate is not re-measurable",
        ),
        what: "`\"panicked at\" during the boot` — and the panic is sshd's, not the kernel's: \
               `sshd: cannot bind 0.0.0.0:22: netd error`, on a boot where netd had already said \
               there is no NIC and exited 0. This test refuses any boot whose console carries a \
               panic, so its own subject is untouched and the red names the workload. \
               `ALONE: GREEN`",
        evidence: "the same ten consecutive suites as the row above, loads 6.4-9.7",
        source: "tests/toyos-rust-tests/src/bin/netd_gone_mid_bind.rs",
        measured: "2026-08-15",
    },
    // ---------------------------------------------------------------------
    // `wt/toyos-ciwall`, dev host, 2026-08-15: the one-accumulator tree's full
    // suite (landed as `81cfe22`), 256 passed and 4 failed, with a second
    // worktree's suite holding guest slots beside it. Two of the four are the
    // same QEMU-exited-0 signature under two names, which is neither test's
    // subject — a shard's partitioning diff cannot reach a guest that never
    // booted. Both are retired since 2026-08-22: that signature is a guest that
    // reset itself during boot, which is the silent death of PR #202's class.
    // ---------------------------------------------------------------------
    Red {
        test: "screen_fatal_halt",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the silent death of PR #202's class (commit 5e74971e), retired 2026-08-22. The \
             harness passes `-no-reboot` (`tests/common/qemu.rs`, whose own comment says a \
             guest that triple-faults exits QEMU), so `QEMU died before ===READY=== (status \
             … 0)` is a guest that reset itself during boot having said nothing: not a kill, \
             which exits on a signal, and not a QEMU that could not start, which exits \
             non-zero. No Ring 0 entry cleared the direction flag, and every `memcpy`/`memset` \
             reached from an interrupt inside `memmove`'s `std` window wrote its bytes below \
             its destination — 37 deaths in 13,960 twelve-wide `bootable.img` boots without \
             the `cld`, 25 of them silent exactly like this, 0 of any kind in 7,418 with it \
             (p = 2.9e-9); a parked silent death reads `RFL=[D--Z-P-]` with a non-canonical \
             RIP (PR #198). Same instrument — TCG, twelve wide, a boot — so the class A/B \
             transfers, and the cold-build correlation the write-up found is the load that \
             raised the rate. A clean exit before the marker now is a new measurement. The \
             arm that reports this exit (`tests/common/qemu.rs`) now includes `seen` and the \
             UART log rather than dropping both",
        ),
        what: "`[qemu] QEMU died before ===READY=== (status: Ok(ExitStatus(unix_wait_status(0))))` \
               — QEMU exited *successfully* before the guest said anything, so the capture holds \
               nothing to bisect and the test's name is the whole of the evidence. \
               `ALONE: GREEN`, and green again in 3 s run by name",
        evidence: "one full `cargo test` on `wt/toyos-ciwall`, in its 106.4 s parallel phase, with \
                   `[host-slots]` naming `toyos-capwin`'s suite on the same host; the run's own \
                   width line was `fastest boot 1380 ms against the reference 1320 ms`, 1.05x, so \
                   this is not the slow-phase shape",
        source: "tests/toyos.rs screen_fatal_halt",
        measured: "2026-08-15",
    },
    Red {
        test: "double_fault_stack",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the identical signature, retired 2026-08-22 with `screen_fatal_halt`'s row above \
             for the identical reason: a guest that reset itself during a loaded boot under \
             `-no-reboot`, the silent death of PR #202's class (commit 5e74971e; 37 deaths in \
             13,960 unfixed boots, 0 in 7,418 fixed)",
        ),
        what: "the identical line, in the same phase as the row above — and the two names have \
               nothing in common but a boot. `ALONE: GREEN`, and green again in 2 s run by name",
        evidence: "the same full `cargo test` on `wt/toyos-ciwall`; the same day the signature also \
                   took `log_backing_read_error` on `wt/toyos-logd56` and, through the screendump \
                   wait rather than the ready marker, `screen_console_shell` on `wt/toyos-capwin`",
        source: "tests/common/qemu.rs double_fault_stack",
        measured: "2026-08-15",
    },
    // ---------------------------------------------------------------------
    // The nightly A/B of the one-accumulator fix: dispatches `31900045901`
    // (`main` at e064a96) and `31900050723` (the same tree plus the fix),
    // twelve KVM shards each, minutes apart on one runner pool. The trees
    // differ only in `src/testargs.rs`, `tests/toyos.rs` and a deleted issue
    // file, so nothing in either guest image moved — what the fix changes is
    // which shard a test lands in.
    // ---------------------------------------------------------------------
    Red {
        test: "screen_diag_boot",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 2),
        standing: Standing::Retired(
            "the string is the whole defect and it is now declared once: \
             `common::volumes::LOG_ON_CONSOLE_AND_FILE` is what the assertion reads, beside \
             `NO_LOG_ALERT`, which its Fast-tier sibling `screen_log_absent` already read that \
             way, and its doc names `report_log_destination` as the writer. The rule the copy \
             broke: a test asserting on a kernel or daemon log line reads that line from one \
             named constant — the writer's own where the crate is reachable, otherwise a single \
             declaration in `tests/common/` that cites the writer — and never a literal copied \
             at the assertion, because a copy has nothing holding it to the sentence it copied \
             and a reworded line then reds a gate that is measuring nothing (2026-08-16)",
        ),
        what: "`\"log: this boot is on the console and in\" is not on screen five seconds after the \
               boot finished`, and `red again` alone in both runs. The mode is not what is broken: \
               the screen printed beside the message carries \
               `log: this boot is on the console and on /log`, the wording `ecede44` gave the line \
               after `9ca7631` took the file off the kernel. A `Tier::Nightly` name, so no pull \
               request runs it, and the nightly's alarm job fires on `schedule` and not on the \
               dispatches these were",
        evidence: "runs 31900045901 (job 95049265216) and 31900050723 (job 95049280299), both \
                   `guest (12)`, read with `gh run view --job`",
        source: "tests/common/volumes.rs",
        measured: "2026-08-15",
    },
    Red {
        test: "boot_partition_identity",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the same fix as the dev-host row above, `f12b684f` / PR #217, retired 2026-08-23. \
             This capture is the one that took the load qualifier off the race — one guest on the \
             machine, `--jobs 1` — and the reading that replaced it explains this shard as well \
             as that session: sshd bound into a teardown already in progress and `hangup` had no \
             arm for either kernel word it met",
        ),
        what: "`\"panicked at\" during the boot` — `sshd: cannot bind 0.0.0.0:22: netd error`, the \
               same panic the dev-host row above carries, on a KVM shard with one guest on the \
               machine. So the \"only above load average 6\" qualifier belongs to that session and \
               not to the race. `ALONE: GREEN, and it was alone both times — a rate and not a \
               classification`",
        evidence: "run 31900050723, job 95049280131 (`guest (3)`), `wt/toyos-ciwall`; green in the \
                   sibling dispatch 31900045901 minutes earlier on the same names and the same \
                   image",
        source: "tests/toyos-rust-tests/src/bin/netd_gone_mid_bind.rs",
        measured: "2026-08-15",
    },
    Red {
        test: "boot_partition_identity",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 4),
        standing: Standing::Retired(
            "**the producer was established after this row was written, and by reading rather \
             than by capturing** — which is why the row's own \"no capture of it can say which \
             one ran\" was right and still did not settle it. `toyos::net::hangup` mapped \
             everything that was not `IpcError::Disconnected` to `NetError::Io`, and the two \
             writes a `tcp_bind` makes ahead of its response read meet \
             `SyscallError::Gone` at `SYS_HANDLE_SEND` and `SyscallError::NotFound` at \
             `SYS_WRITE` once `port::Acceptor::on_zero_handles` has run. Both are now \
             `NetError::NetdNotFound`. Which of the two ran is still not decidable and no longer \
             needs to be: they are two syscalls of one `send_with_handles`, microseconds apart, \
             and one change fixes both. Landed `f12b684f`, PR #217; gated by `netd_gone_mid_bind` \
             (PR #218), whose second and third arms are red without it. Retired 2026-08-23",
        ),
        what: "**the same signature, and the producer is not established** — `sshd: cannot bind \
               0.0.0.0:22: netd error` at `sshd/src/main.rs:359:23`, the identical bytes to the \
               row above. That is as far as the message goes and this row goes no further: the \
               write-up's own finding is that the std fork flattens every `io::Error` kind to \
               the string `netd error`, so four candidate paths print this line and no capture \
               of it can say which one ran. **What matches beyond the message is the timing \
               shape**, which is the part worth recording: `spawn: /bin/sshd pid=6` at 0.559 s \
               and `exit: netd pid=5 code=0` at 0.566 s, so the bind went into a teardown \
               already in progress — the same direction as the earlier CI capture, where the \
               gap was 23 ms, and the opposite of the clean-exit arm in the same write-up, \
               where sshd started after netd was gone. `ALONE: GREEN, and it was alone both \
               times — nothing the harness controls differed, so it failed once and passed \
               once. That is a rate and not a classification`. Shard 1's other 173 names passed",
        evidence: "run 32044008591, job 95428160739 (`guest (1)`), PR #116 on \
                   `wt/toyos-invariantp` — a diff of documentation and `src/redlist.rs` strings \
                   with no code in it at all, which is what says the race is not the branch's. \
                   The denominator is this branch's four CI runs that reached a verdict — \
                   32043101865, 32044008591, 32044756253 and 32047352064 — of which only the \
                   second was red under this name; a fifth (32044676027) was cancelled and says \
                   nothing",
        source: "tests/toyos-rust-tests/src/bin/netd_gone_mid_bind.rs",
        measured: "2026-08-17",
    },
    Red {
        test: "handle_kill_policy",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 4),
        standing: Standing::Stands,
        what: "`16 more killed processes left more live objects behind: [(\"Process\", 6, 7)]` — \
               `the_kills_release_what_they_held`'s machine-wide live-object census, one \
               `Process` higher on the second sample than the first. `ALONE: GREEN, and it was \
               alone both times — nothing the harness controls differed, so it failed once and \
               passed once. That is a rate and not a classification`; shard 2's other twelve \
               names passed. **The first CI sighting of a signature recorded so far only on the \
               dev host**, where the write-up added it the same day at 1 of 6 and recorded it \
               green on all twelve KVM shards of that tree. That bears on its explanation \
               rather than its severity: the dev-host bullet leans partly on other *suites* \
               sharing the machine, and a CI shard is one guest per machine with `--jobs 1`, so \
               what survives here is the other half of it — a machine-wide census taken on a \
               shared boot, perturbed by a co-resident test's reap that had not landed yet. \
               **Consistent with that mechanism, not established as it**: nothing in this \
               capture identifies which process the extra `Process` object belonged to",
        evidence: "run 32047352064, job 95438242676 (`guest (2)`), `wt/toyos-invariantp` at its \
                   merge of `origin/main`, so the tree carries main's own commits as well as \
                   this branch's. **Not this branch's code, and that is checkable rather than \
                   asserted**: the only kernel code this branch adds is behind `sched-check` \
                   (forwarding to `toyos-sched/check`), `handle_kill_policy` boots no such \
                   kernel, and `src/build.rs`'s artifact gate measures 0 of 3 check-build \
                   literals in the shipping image on every build. Same denominator as the row \
                   above: this branch's four CI runs that reached a verdict, red in one",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-17",
    },
    Red {
        test: "handle_transfer",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`handle transfer left more live objects behind: [(\"PipeRead\", 2, 3)]` — the \
               per-kind census one `PipeRead` higher on the closing reading, red again in the \
               shard's own alone re-run inside the same shared boot. The deferred-release \
               mechanism through the census instrument: `PipeReadEnd` is a `deferred` row whose \
               only release site is `on_zero_handles`, and a batch in flight on another CPU is \
               live to a census and absent to the queue. The fourth witness of that mechanism \
               and the first through this name on the hosted shard; the pull request it red on \
               changes comments only, byte-identical code, checkable in its own diff",
        evidence: "run 32876917304, job 97897004452 (`guest (3)`), `wt/toyos-sw5` — the prose \
                   sweep's tier-two batch, whose PR body carries the filtered-diff-is-empty \
                   proof. Parallel arm red at 17:22:04, the alone re-run red eight seconds \
                   later in the same boot",
        source: "issues/kernel/deferred-release-outlives-its-syscall.md",
        measured: "2026-08-25",
    },
    Red {
        test: "usb_boot_stick_pulled",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`\"PANIC:\" after the boot stick was pulled` — and the panic is the kernel's: \
               `a task waits on at most one queue` (`toyos-sched/src/waitq.rs:124`) reached through \
               `Ticket::register` from `kernel::io_uring::enter`, in logd's `Poller::submit`, \
               4.7 s after the stick went and its writes started failing. Not the `sys_read` \
               keyboard-flood path already written up under that assertion. The machine did not \
               halt — the capture runs on to `pull-probe-91`. `ALONE: GREEN, and it was alone both \
               times — a rate and not a classification`",
        evidence: "run 31900050723, job 95049280131 (`guest (3)`), the serial phase; green in the \
                   sibling dispatch 31900045901 on the byte-identical kernel",
        source: "issues/kernel/io-uring-enter-trips-the-one-queue-invariant.md",
        measured: "2026-08-15",
    },
    // ---------------------------------------------------------------------
    // This documentation branch's own pull-request run, adjudicated here
    // rather than re-run: the diff is prose, one caveat and this table, and
    // reaches nothing that boots.
    // ---------------------------------------------------------------------
    Red {
        test: "null_sink_client_exits",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the number was never the race and it is unchanged: `settle_null_sink_client_exits` \
             (`tests/toyos.rs`) waits for both removals on the guest's own liveness, between the \
             test and its check, so the window no longer closes on the line soundd writes about \
             the exit that closed it. `expect: 2` stays exact — the departure vocabulary is \
             asserted per removal",
        ),
        what: "`soundd reported 1 client removals, expected 2` — and the capture shows the second \
               `soundd: client 0 removed (closed)` never arriving rather than arriving wrong: the \
               guest printed `null sink drained two clients in series` and exited, which is where \
               the window ends. Round 1's removal made it in because a whole second round followed \
               it. `ALONE: GREEN, and it was alone both times — a rate and not a classification`",
        evidence: "PR #85 run 31904338273, job 95059750268 (`guest (1)`), on a branch of \
                   documentation and this table",
        source: "tests/toyos.rs",
        measured: "2026-08-15",
    },
    // ---------------------------------------------------------------------
    // PR #94 (`wt/toyos-schedfuture`), run 31944633004, 2026-08-16: five
    // documentation files, two reds, both adjudicated here and fixed at their
    // owners rather than re-run.
    // ---------------------------------------------------------------------
    Red {
        test: "null_sink_client_exits",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "`settle_null_sink_client_exits` (`tests/toyos.rs`), landed with this row",
        ),
        what: "`soundd reported 1 client removals, expected 2` again, and **`ALONE: red again — \
               the defect is real`** where PR #85's occurrence went green: one guest on a KVM \
               runner with nothing to contend with \
               reproduces it, so the wide phase was never what produced it. Both captures carry \
               one removal and one `clients=0` — soundd flushes the window in the same mix-loop \
               iteration that prints the removal, so the `clients=` statistic the write-up offered \
               as the other way to buy non-vacuity is on the far side of the same close",
        evidence: "PR #94 run 31944633004, job 95158684501 (`guest (1)`), `wakes=484` in the wide \
                   run and `wakes=481` in the isolated re-run",
        source: "tests/toyos.rs",
        measured: "2026-08-16",
    },
    Red {
        test: "i8042_undecoded_bytes",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the verdict revises itself once, 2026-08-28. A mute line said while a decoder \
             still holds the run is `HEALTH_MUTE_BLIND`, and the first blamed byte moves it to \
             `HEALTH_MUTE_SAID` with the line that names the bytes \
             (`kernel/src/drivers/i8042/mod.rs`). The interleaving this row records — the \
             verdict out after four of Pause's six bytes — is no longer waited for: \
             `i8042-split-burst` stages it on every run of `i8042_undecoded_bytes`, whose first \
             mute line must name nothing and whose second must name the sequence. Control \
             measured both ways: with the revision reverted and the stage kept, the test reds \
             on this row's line shape; with it, green",
        ),
        what: "`the line names no byte: [kernel 2.494 cpu1] i8042: 1 interrupts and 4 bytes, \
               nothing decoded — first seen at 2494ms`. **Four bytes and not zero, so this is a \
               different producer from the two dev-host rows under this name**: it is the test's \
               own Pause, reported \
               after the first interrupt delivered four of its six bytes, with the decoder's run \
               still open and `Unexplained` therefore empty. Neither half that landed for those \
               rows reaches it — the line is after the injection and the interrupt carried bytes. \
               `ALONE: GREEN, and it was alone both times — a rate and not a classification`",
        evidence: "PR #94 run 31944633004, job 95158684534 (`guest (2)`); the isolated re-run in \
                   the same job reported the whole sequence, `2 interrupts and 6 bytes … no event \
                   from [0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5]`",
        source: "tests/toyos.rs",
        measured: "2026-08-16",
    },
    Red {
        test: "screen_console_shell",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`no \\`i8042:\\` line above the prompt: \\`/boot/toyos/kernel.log\\` never reached the \
               scrollback` — **and the panel it printed disproves that sentence**: every line on \
               it is stamped `0.000` and comes from the first screenful of the boot, so the seed \
               reached the console and the view was at its *head*. `ALONE: GREEN, and it was \
               alone both times — a rate and not a classification`. **Not about the diff it was \
               found on**, which is the i8042 interrupt tally: that change writes no boot line \
               and removes none, so the set of `i8042:` lines this test looks for is identical \
               either side of it. **Both halves of the test have moved since.** The wait is now \
               for the prompt *and* the seed's witness, where `screendump_while` stopped at the \
               first frame carrying the prompt and nothing ordered the seed's paint against it; \
               and the message no longer names a cause it did not establish — it reads the byte \
               count `console: ready` publishes and says whether the log reached the scrollback \
               at all. A recurrence therefore arrives already told apart, and would be a view \
               that is not at the bottom rather than a console that started blank",
        evidence: "PR #111 run 32040411208, job 95418635461 (`guest (3)`); the isolated re-run in \
                   the same job was green",
        source: "tests/toyos.rs",
        measured: "2026-08-17",
    },
    // ---------------------------------------------------------------------
    // PR #128 run 32249152467, job `guest (2)`, 2026-08-19. Shard 2/12 at
    // `--jobs 1 --host-slots 0` — the log reads `--- parallel, 1 wide ---`, so
    // one guest on the machine and no contention to appeal to. **Two i8042
    // names in one shard, in one phase, and a first sighting for both**:
    // `--known-red` answered `NOT ON THE LIST` for each. New names, so they get
    // rows of their own rather than joining the family's existing ones — the
    // undecoded-bytes rows are a different producer and merging would make
    // three failures read as one. Each was re-run as its group and passed
    // twice, and the run is red on the rate.
    // ---------------------------------------------------------------------
    Red {
        test: "i8042_keyboard",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the script outran QEMU's sixteen-byte PS/2 queue. 26 set-1 bytes went out on a \
             `thread::sleep` clock, so the bound held only while the guest kept draining, and \
             past the queue `ps2_queue()` drops one byte at a time and says nothing — a lost \
             make takes its break with it (`handle_key` queues nothing for a usage nothing \
             holds), which is exactly `0x29` missing entirely, and a lost `0xE0` leaves a press \
             with no release, which is exactly the `usage 0x50: 1 presses, 0 releases` the \
             isolated re-run produced. Reproduced deterministically by putting the same 26 bytes \
             into one `input-send-event`: `i8042: drain bytes=16 keys=15` and `0 dropped, 0 \
             overruns, 0 lost edges`. The test is paced against the guest's own `kev` lines now \
             — one group outstanding, four bytes at most",
        ),
        what: "`no event for HID usage 0x29 in [KeyLine { usage: 11, modifiers: 0, translated: \
               \"h\" }, …]` — twenty `KeyLine`s carrying the rest of the scripted sequence and \
               translating it: `h e l l o`, shift-`B` (`usage: 5`, `modifiers: 1`, `\"B\"`), then \
               `usage: 80` → `\\u{1b}[D` and `usage: 77` → `\\u{1b}[F`. Escape is `0x29` and is \
               nowhere. **One structural oddity, recorded and not read as a cause**: the second \
               `usage: 225` press/release pair encloses no key event, where the first encloses \
               the `B`. `ALONE: GREEN, and it was alone both times — a rate and not a \
               classification`",
        evidence: "PR #128 run 32249152467, job `guest (2)`, shard 2/12 at `--jobs 1`; re-run as \
                   its group twice in the same job, `PASS (5s)` and `PASS (6s)`. In the same \
                   phase `i8042_mouse` passed with `0 keys, 0 undecoded` in its tally where both \
                   re-run boots reported `28 keys, 12 undecoded` — three separate boots, so an \
                   accompanying observation and not a shared-guest claim",
        source: "tests/toyos.rs QEMU_PS2_QUEUE",
        measured: "2026-08-19",
    },
    Red {
        test: "i8042_no_spurious_wake",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the same over-subscription — twenty bytes against a sixteen-byte queue — and the \
             same missing bound. What a drain carries is whatever the ISR found in the ring, so \
             a host injecting on a wall clock was asserting on a batching it did not control: \
             the capture's own `bytes=8 keys=2` is the Pause and the key that followed it 50 ms \
             later taken together, which is a guest that did not drain for 50 ms. Each piece is \
             paid for now before the next goes out — a Pause by a drain the driver logged, a key \
             by its two `kev` lines — so the zero-event drain is arranged rather than hoped for",
        ),
        what: "`no drain produced zero events — the stimulus never landed` — **and the capture it \
               prints contradicts its second clause**: the kernel names all six bytes of the \
               test's own Pause, `no event from [0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5]`, so the \
               stimulus landed. What is missing is a drain carrying *only* it — the drain that \
               took it reports `bytes=8 keys=2` and the next `bytes=12 keys=4`, so neither has \
               zero events. Alone the same test reports `2 zero-event drains, none woke; 3 real \
               ones, all did`. Whether a real key byte sharing that drain is the instrument's \
               fault or the batching's is **not** decided here. `ALONE: GREEN, and it was alone \
               both times — a rate and not a classification`",
        evidence: "PR #128 run 32249152467, job `guest (2)`, the same shard and phase as this \
                   run's `i8042_keyboard` row; re-run as its group twice in the same job, \
                   `PASS (227ms)` and `PASS (222ms)`",
        source: "tests/toyos.rs QEMU_PS2_QUEUE",
        measured: "2026-08-19",
    },
    Red {
        test: "screen_console_panic",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "**the panic path was never in it, and the row's own capture says so.** The job log \
             prints the panel, and the line at the prompt reads `test_rs_TESTpanic_child 3` — the \
             shell answered `not found`, so no panic was ever asked for and there was no report \
             to take any screen. What mangled the command is QEMU's 16-byte PS/2 queue, which \
             drops what a guest that is not draining cannot take, silently and one byte at a \
             time: the lost shift break is why four letters came back capitalised. The host was \
             typing on a wall clock, which `QEMU_PS2_QUEUE`'s own doc had already ruled out for \
             every other injection in this suite. `console_type_line` now sends the line in \
             bursts no wider than that queue and waits for the panel to echo each one, so a burst \
             always starts against an empty queue: staged at the limit (the whole line in one \
             transmission) that is 5 of 5 red before and 0 of 5 after",
        ),
        what: "`the fatal report never took the screen back from the console — which would make \
               /bin/console a downgrade on the machine it is for`, at 96 s against the suite's \
               usual seconds, so the shape is a handoff waited for and never observed. First \
               sighting: `--known-red` answered `NOT ON THE LIST`. **Not about the diff it was \
               found on**, which is PR #141's merge-queue package — workflow triggers and \
               CLAUDE.md prose, no kernel byte. `ALONE: GREEN, and it was alone both times — \
               nothing the harness controls differed, so it failed once and passed once. That is \
               a rate and not a classification`",
        evidence: "PR #141 run 32306139422, job 96239259411 (`guest (3)`), 2026-08-19; the \
                   isolated re-run in the same job was green",
        source: "tests/toyos.rs console_type_line",
        measured: "2026-08-19",
    },
    Red {
        test: "screen_console_panic",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the same keystroke loss as the row above, and the capture that settles it: the panel \
             read `/home/root> test_rspanic_child 3`, which is the first sixteen set-1 bytes of \
             what was typed and then a hole exactly one queue wide. Two sightings, two mangled \
             command lines, no panic in either",
        ),
        what: "`the fatal report never took the screen back from the console`, at 181 s. Never on \
               the list — recorded here because it is the sighting whose capture names the cause, \
               and because a row that retires a name has to account for every red under it. \
               **Not about the diff it was found on**, a FAT32 cluster-release change. `ALONE: \
               GREEN`",
        evidence: "PR #262 run 32667714627, job 97263796784 (`guest (10)`), 2026-08-23; the \
                   isolated re-run in the same job was green in 6 s",
        source: "tests/toyos.rs console_type_line",
        measured: "2026-08-23",
    },
    Red {
        test: "log_poll_outlives_a_close",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`the close probe exited Some(1)`, and the probe's own line: `log-close: FAILED: \
               the poll outlived the close and then never completed on a record either, so what \
               it outlived may have been its own arming`. `ALONE … GREEN`, so the harness \
               classifies it as `Sched::Parallel` and the run stays red on that. **A second row \
               under this name and not a correction of the first**: the row above is a \
               `DOUBLE PANIC` and nothing disputes it, while this is the probe returning a \
               verdict the guest survived. It also spends the other row's retirement clause — \
               `three loaded suites of the fixed tree with no red under this name` — because \
               this loaded suite went red under it",
        evidence: "one full `cargo test` on the dev host 2026-09-01, `w5b5-host-build` at \
                   2617bab9, host at 1.05x width; 300 passed, 1 failed, 301 total (396.3s), and \
                   the harness's own `ALONE` re-run green",
        source: "issues/panic-path/a-double-panic-at-boots-edge-says-nothing-but-its-name.md",
        measured: "2026-09-01",
    },
    Red {
        test: "log_poll_outlives_a_close",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`kernel panic: DOUBLE PANIC — the guest went quiet because every CPU is halted, \
               not because it was still working. The panic is the finding and the guard never \
               got to be one`. The kernel's complete last words were `[kernel 0.991 cpu0] DOUBLE \
               PANIC` — no first-panic text, no location, which is the second finding. `ALONE: \
               GREEN — it fails only beside other guests`; the load was two worktrees' full \
               suites interleaved over the shared twelve guest slots. **Not about the diff it \
               was found on**, a census-settling change inside `handle_kill_policy`'s own guest \
               binary. **2026-08-22:** the leading explanation is PR #202's class — no Ring 0 \
               entry cleared `DF`, and a machine-wide death at boot's edge under two suites on \
               a branch with no kernel byte is that class's shape (37 deaths in 13,960 loaded \
               boots before the `cld`, 0 in 7,418 after, every one in the spawn burst this \
               0.991 s sits in); `kernel/src/panic.rs` now names the first crash under a \
               `DOUBLE PANIC`, so the next sighting says whether it was a fault. Not shown \
               here, so the row stands. One sighting in one loaded suite; retires at three \
               loaded suites of the fixed tree with no red under this name (p = e^-3). That \
               count is owed: on 2026-08-22 the guest suite was refused behind \
               `wt/toyos-census`'s sysroot claim (#209)",
        evidence: "dev host, 2026-08-19 22:21 UTC, `cargo test` in wt/toyos-hkpfix beside \
                   wt/toyos-freshness's suite; 267 of 268 passed, this one red at 25 s in the \
                   parallel phase, green alone in the same run",
        source: "issues/panic-path/a-double-panic-at-boots-edge-says-nothing-but-its-name.md",
        measured: "2026-08-19",
    },
    Red {
        test: "screen_console_clear",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the one mechanism that produces this sentence was named and closed. \
             `screen_console_clear` types both its commands through `console_type_line` \
             (`tests/toyos.rs`), which splits a line into bursts no wider than QEMU's PS/2 \
             queue and waits for the console to echo each one, so a keystroke can no longer \
             be dropped between the host and the shell; staged the other way — the whole \
             command in one transmission — the test printed this verdict verbatim on 3 of 3 \
             dev-host runs. `7a033450` is the change. What that does not settle is whether \
             THIS sighting was that: its capture was not kept, and a lost pixel and a mangled \
             command name print the same sentence. The next sighting is unambiguous, because \
             the command reaching the shell is now checked before the panel is",
        ),
        what: "`the graffiti actuator did not reach the panel: 0 of 2073600 pixels are \
               [0, 192, 0] and the 8px strip below the cells is not`, at 127 s against a \
               fast-tier price — a panel that never received the write inside a window two \
               orders above its cost, not a wrong pixel. The same run's `durations` job then \
               refused the 126,762 ms reading against the 10,000 ms line, correctly: that \
               number is this stall and must never be committed as a price. First sighting; \
               same evening and same shape as `screen_console_panic`'s row — composition \
               under a loaded host loses the panel's update. **Not about the diff it was \
               found on**, a tier declaration and a duration table (PR #135). `ALONE: GREEN, \
               and it was alone both times`",
        evidence: "PR #135 run 32303408773, job 96231120463 (`guest (11)`), 2026-08-19; the \
                   isolated re-run in the same job was green",
        source: "tests/toyos.rs",
        measured: "2026-08-19",
    },
    Red {
        test: "console_line_atomicity",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`writer A declared 1000 whole lines and the capture carries 995` — five lines \
               of one writer's thousand missing from the shared capture, the name's first \
               sighting on THIS instrument: its standing rows are the loaded dev host's \
               (fires 1 of 3 there), and CI runs one guest per machine, so whatever loses \
               the lines here is not host contention. `ALONE: GREEN, and it was alone both \
               times — a rate and not a classification`. **Not about the diff it was found \
               on**, an issues-and-prose capability audit (PR #166)",
        evidence: "PR #166 run 32364721784, job 96411690231 (`guest (10)`), 2026-08-20; the \
                   isolated re-run in the same job was green",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-20",
    },
    Red {
        test: "syscall_window_nmi",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`the storm never reported — is `syscall-window-nmi` on?` at 1,505 s against a \
               committed 6,825 ms, in a 288-name run at 92 guests with a second worktree's \
               suite on the same host. A 220x wall stretch, and the guest's own message for \
               a storm line that has not arrived yet. The isolated re-run in the same \
               session was green in 5 s and reported `3000 sent, 3000 taken, 43 in the \
               window`. First sighting, no denominator; `--known-red` answered NOT ON THE \
               LIST. **Not about the diff it was found on**",
        evidence: "dev host, 2026-08-27, the `cargo test` run of the md2 defect-fix branch; \
                   `exit_wait_storm` reds in the same phase and is already on this list",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-27",
    },
    Red {
        test: "exit_wait_storm",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`timed out after 12s, with the guest still talking 13s ago (245 console \
               line(s) while it ran) — it was working and did not finish`, and the same \
               run's `durations` job refused the 13,058 ms reading against the 10,000 ms \
               line — correctly: the committed price is 200 ms, so the number is the \
               partition's co-scheduling, a 65x wall stretch on a storm of exiting \
               children, not the test's own cost. First sighting; `--known-red` answered \
               NOT ON THE LIST. **Not about the diff it was found on**, one issue file \
               (PR #147)",
        evidence: "PR #147 run 32331741273, job 96313605393 (`guest (1)`), 2026-08-20; the \
                   same run's `guest (11)` red on `screen_console_clear`, that family's \
                   second CI sighting",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-20",
    },
    Red {
        test: "tlb_shootdown_waits",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`exit code 101` at 53 ms — the guest binary's own assertion, and the \
               2026-08-20 lock-conversion pass already recorded the sharper point when the \
               same name red on a loaded dev host, ALONE: GREEN: the assertion that fires \
               is the test's own *control*, so it is the one assertion in the suite that \
               cannot tell a slow host from a broken measurement. **Not about the diff it \
               was found on**, two issue files and a tests/CLAUDE.md bullet (PR #150)",
        evidence: "PR #150 run 32334225614, job 96320634405 (`guest (5)`), 2026-08-20; the \
                   dev-host sighting the same night is in the source issue",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-20",
    },
    // ---------------------------------------------------------------------
    // `wt/toyos-purecrates`, dev host, 2026-08-18: three full `cargo test` runs
    // in one session, on a branch whose whole delta is three kernel files
    // moving into two pure crates with no line of their logic changed — every
    // moved file `diff`s against its original as doc comments, one `use` path
    // and two test paths. All twelve KVM shards of that commit are green
    // (PR #124). Three runs, three different names red, each `ALONE … GREEN`,
    // and each red twice over on the second and third: this is the dev-host
    // load family and the rows say so. Adjudicated here rather than re-run
    // away, per the root CLAUDE.md — each of these answered `NOT ON THE LIST`
    // when it was asked, which is the gap this campaign closes.
    // ---------------------------------------------------------------------
    Red {
        test: "console_line_atomicity",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 3),
        standing: Standing::Stands,
        what: "`kernel panic: DOUBLE FAULT on CPU 1 (pid=Some(Pid(2)) tid=Some(Tid(0)))`, 21 s \
               against the 9 s it passed in the run before. **A kernel death and not a verdict**, \
               and the first dev-host one the panic vocabulary has named rather than reported as \
               silence. `ALONE … GREEN`, which that vocabulary's own write-up says is not evidence \
               against a panic — one reached under contention does not reproduce alone either. \
               The kernel's report is not in *this* record: the arm printed \
               `tail(&result.stdout)`, which is the userland half of the capture, and the report \
               sat unread in `TestResult::serial`. That is closed at the field rather than at the \
               arm — `TestResult::error` is a `WaitVerdict`, which cannot be built without the \
               capture it was reached on — so the next sighting arrives with `cr2`, the page \
               walk, the backtrace and `[ist1] used N of M` under the sentence. **2026-08-22:** \
               the leading explanation is PR #202's class — no Ring 0 entry cleared `DF`, so an \
               interrupt inside `memmove`'s `std` window made every later `memcpy`/`memset` \
               write below its destination; a kernel stack or a `KernelCtx` written that way \
               is a `#DF` on the next push, and the class's parked deaths carry non-canonical \
               RIPs and a null `ss` (PR #198) — 37 deaths in 13,960 loaded boots before the \
               `cld`, 0 in 7,418 after, twelve wide on TCG like this suite. Not shown: the \
               report this sighting never printed is the only thing that could, so the row \
               stands. Prior rate 1 of 3 loaded suites; retires at nine loaded suites of the \
               fixed tree with no red under this name (p = e^-3). That count is owed: on \
               2026-08-22 the guest suite was refused behind `wt/toyos-census`'s sysroot \
               claim (#209)",
        evidence: "three full `cargo test` runs in one session on `wt/toyos-purecrates`, twelve \
                   wide, `fastest boot 1522 ms against the reference 1320 ms` on the run that \
                   red",
        source: "issues/kernel/a-double-fault-on-cpu-1-under-a-wide-suite.md",
        measured: "2026-08-18",
    },
    Red {
        test: "console_line_atomicity",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 3),
        standing: Standing::Stands,
        what: "`writer A declared 1000 whole lines and the capture carries 798` — the \
               non-vacuity count, not the atomicity assertion: **`0 mixed` means the \
               mechanism held**. A second sighting of the 2026-08-15 capture loss with the \
               other writer and a different count, `ALONE … GREEN`. The writers number \
               their lines now, so a fresh red of this kind names the loss itself — a gap \
               inside the numbered run, or a contiguous run missing its tail",
        evidence: "the same session's third run, twelve wide, `fastest boot 1381 ms against the \
                   reference 1320 ms`",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-18",
    },
    Red {
        test: "handle_kill_policy",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 3),
        standing: Standing::Stands,
        what: "`16 more killed processes left more live objects behind: [(\"Process\", 6, 7)]` — \
               byte-identical to the 2026-08-17 sighting on an unrelated branch, numbers \
               included, which is what a machine-wide census either side of a kill on a shared \
               boot is expected to do. `cargo test --test toyos-build -- handle_kill_policy` on \
               the same tree straight afterwards: `PASS handle_kill_policy (615ms)`",
        evidence: "the same session's first run, twelve wide",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-18",
    },
    // ---------------------------------------------------------------------
    // `wt/toyos-spawnrule`, dev host, 2026-08-19: three full `cargo test` runs
    // in one session on a branch whose whole behaviour change is one line of
    // `SYS_SPAWN`'s slot-map resolution. **Two kernel deaths and one clean
    // run**, at three different host widths: 1.02x red, 1.07x green 268 of 268,
    // 1.41x red — and the kernel source under the first and third differs from
    // the green one's by comments alone, so no statement compiled differently
    // between them.
    //
    // The first of those two deaths was `process_lifecycle`'s Ring 0 fetch at
    // `0x0` inside `SYS_READ`, and its row is gone with the defect: it was a
    // `context_switch` restoring a task another CPU was still standing on, and
    // a red under that name now is a new measurement rather than this one. The
    // second is the row below, and it was the last of the session still
    // unaccounted for; it is the same defect, and the row's `Retired` reason
    // carries what decided that.
    // ---------------------------------------------------------------------
    Red {
        test: "sched_stress",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 3),
        standing: Standing::Retired(
            "the same defect as the four Ring 0 fetches filed beside it, and it was fixed \
             before this row was ever re-measured: `navigate.rs:161` is `init_front().unwrap()`, \
             which `BTreeMap::iter` reaches only when the map's `root` reads `None` and its \
             `length` does not — a pair no sequence of inserts and removes produces, so the \
             panic reports corruption of the record rather than a scheduler decision. That \
             record is `CpuSched` in `static SCHEDS`, and `SchedPass`'s own `&mut CpuSched` is \
             a local on the kernel stack the pre-fix `pop_surplus` handed to two CPUs at once, \
             a few hundred instructions before `apply_timer` walks the map. Measured \
             2026-08-20, same host, one-word A/B: 0 kernel deaths in 3,600 boots on this tree, \
             2 in 3,120 with `pop_surplus(None)` restored — both of them `cpu 7 has no \
             CpuSched` at `sched/driver.rs:219`, the same static reading as a value only a \
             stray write produces, and both after cpu7 had already completed a pass. 20 loaded \
             `sched_stress` runs green at 4.27x-8.00x host width against the 1.41x that took \
             this one. A red under this name now is a new measurement — and the two \
             sightings that arrived after this row are accounted for: both are the \
             direction-flag stray writer, and the T14 re-measurement that closed them \
             took 17,555 KVM boots across four arms without one death of any kind",
        ),
        what: "`QEMU disconnected` — the kernel panicked at \
               `alloc/src/collections/btree/navigate.rs:161`, `Option::unwrap()` on `None` \
               **inside `BTreeMap`'s own immutable iterator**, walking a CPU's `parked` map \
               from `SchedPass::apply_timer`. A map whose length disagrees with its nodes, not \
               an absent deadline. It took the shared boot with it: 129 further names in the \
               same run answered `Failed to flush QEMU stdin: … BrokenPipe`, so 130 of that \
               run's reds are one event and only this one is a measurement",
        evidence: "the same session's third run and the most loaded of the three, `fastest boot \
                   1867 ms against the reference 1320 ms`; `ALONE sched_stress: GREEN` and \
                   `PASS (2s)` in the same run",
        source: "src/redlist.rs",
        measured: "2026-08-19",
    },
    // ---------------------------------------------------------------------
    // The same session's last two runs, after the branch merged `origin/main`
    // at `bf54143`. Both red, both `ALONE … GREEN`, neither about the diff.
    // The first was `i8042_kbd_echo`'s Ring 0 fetch at `0x1b`, and its row is
    // gone with the defect for the reason the block above gives. The second is
    // below, and it is the only one of that session's five reds that was never
    // a kernel death.
    // ---------------------------------------------------------------------
    Red {
        test: "screen_console_shell",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 2),
        standing: Standing::Stands,
        what: "`typed \\`echo zqjxk\\` at the prompt and no row of the panel is its output` — a \
               **different assertion** from this name's 2026-08-17 CI row, which is about the \
               seeded `i8042:` line. 786 s against `PASS (2s)` alone in the same run, and the \
               panel it decoded carries only the first frames of boot, so the guest never \
               reached the prompt inside the window. The one red of this session's five that \
               is not a kernel death",
        evidence: "the fifth run of the same session, `fastest boot 1622 ms against the \
                   reference 1320 ms`, 1.23x width; `ALONE screen_console_shell: GREEN`",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-19",
    },
    // Sighting on `ci-qemu-pin`, whose whole delta is `.github` and
    // `src/ci.rs`. Adjudicated here rather than re-run away.
    Red {
        test: "console_locale_detect",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 2),
        standing: Standing::Disputed(TYPING_UNATTRIBUTED),
        what: "`FAIL console_locale_detect: 10 typed lines and none of them came back`, green on \
               the alone re-run: `ALONE: GREEN, and it was alone both times — nothing the harness \
               controls differed, so it failed once and passed once. That is a rate and not a \
               classification`. **The row below retired this name against `shell_type_line` \
               (`7a033450`) and that fix is in the tree**: the message quoted here is that fix's \
               own verdict, reporting that none of ten lines echoed back. A bounded burst losing \
               bytes at a queue boundary explains a mangled line, not ten of ten each retried \
               three times, so the retired mechanism does not account for this and the \
               retirement is not evidence against it. Not about the diff it appeared on — no \
               kernel, userland or harness byte, and the guest ran the declared QEMU.",
        evidence: "pull-request `ci` run 33411831704, job 99553283770 (`guest (1)`), headSha \
                   30918d0e, 2026-08-31",
        source: "tests/toyos.rs shell_type_once",
        measured: "2026-08-31",
    },
    // The same sentence under the same call site, on another branch. Its own
    // row and not a fold: one name is one row's subject.
    Red {
        test: "desktop_locale_detect",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 2),
        standing: Standing::Disputed(TYPING_UNATTRIBUTED),
        what: "`FAIL desktop_locale_detect: 10 typed lines and none of them came back`, the \
               sentence `console_locale_detect` failed with hours earlier, and green on the \
               alone re-run: `ALONE: GREEN, and it was alone both times — nothing the harness \
               controls differed, so it failed once and passed once. That is a rate and not a \
               classification`. **One call site, one string**: `shell_answers` types `echo \
               surface-up-zqjxk` and `shell_echoes` says this after ten attempts, so the two \
               names differ only in which surface owner is behind the shell. `shell_type_once` \
               sends all three of that line's bursts and its Enter back to back with no \
               guest-side wait — 44 set-1 bytes against the 16 the device holds — which is a \
               defect in the code whether or not it is this sighting's cause; the counter that \
               separates dropped bytes from a shell that was not reading was recorded by \
               neither sighting. Not about the diff it appeared on, a pipe-`Gone` rename \
               touching neither typing nor the terminal.",
        evidence: "pull-request `ci` run 33426887418, job 99613902394 (`guest (9)`), headSha \
                   28be5a85, 2026-08-31; the failure body carries two `compositor: frames=` \
                   lines and nothing from the shell",
        source: "tests/toyos.rs shell_type_once",
        measured: "2026-08-31",
    },
    // A rate on a shared shard, adjudicated here rather than re-run away. Not
    // the typing family: it shares a day and an `ALONE: GREEN` with the two
    // rows above and nothing else.
    Red {
        test: "poll_wake_pipe",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the assertion it fired on is deleted: the guest binary no longer bounds the run's \
             wall clock, only its wake count",
        ),
        what: "`the 300 rounds took 3.010175191s, past the 3s bound` — the row above's own \
               message, 10.2 ms over where CI was 7.2 ms over, and `ALONE … GREEN`. **A second \
               instrument and not a second defect**: the row above is `Instrument::Ci`, which \
               sees one guest per machine, so it could not have said whether a loaded dev host \
               produces the same overshoot. It does, at 1.02x width, which is a quiet host — so \
               the bound is not a contention ceiling either",
        evidence: "one full `cargo test` on the dev host 2026-09-02, `w5b5-host-build` at \
                   71f25c0a, `fastest boot 1353 ms against the reference 1320 ms`; 299 passed, \
                   1 failed, 300 total (214.9s)",
        source: "tests/toyos-rust-tests/src/bin/poll_wake_pipe.rs",
        measured: "2026-09-02",
    },
    Red {
        test: "poll_wake_pipe",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 2),
        standing: Standing::Retired(
            "the assertion it fired on is deleted: the guest binary no longer bounds the run's \
             wall clock, only its wake count",
        ),
        what: "`the 300 rounds took 3.007165755s, past the 3s bound — a wake was slow enough to \
               be a lost one recovered by a later edge`, then `PASS poll_wake_pipe (1s)` alone \
               in the same job. **No wake was lost.** The test owed two assertions and the \
               lost-wake one passed: all 300 edges woke the armed ring, and what failed is a \
               `const BOUND: Duration = Duration::from_secs(3)` inside the guest binary, missed \
               by 7.2 ms — 0.24%. Nothing widened it: the same job priced its host at `fastest \
               boot 1890 ms against the reference 1320 ms — liveness ceilings paid at 1.43x \
               width` over `4 core(s)`, and that factor reaches every host-side ceiling and not \
               that one. First sighting: `--known-red` answered `NOT ON THE LIST`. Not about \
               the diff it appeared on, a `NamespaceBuild` flags word touching neither the pipe \
               nor the poller.",
        evidence: "pull-request `ci` run 33429908117, job 99613928630 (`guest (1)`), headSha \
                   bd533bd0, 2026-08-31; the shard was otherwise green at 196 passed, 1 failed, \
                   197 total (104.2s)",
        source: "tests/toyos-rust-tests/src/bin/poll_wake_pipe.rs",
        measured: "2026-08-31",
    },
    // The three jobs after the row above, on two branches and two shards that
    // touch neither pipe nor poller: 7 reds of 8 attempts over the four jobs.
    Red {
        test: "poll_wake_pipe",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 2),
        standing: Standing::Retired(
            "the assertion it fired on is deleted: the guest binary no longer bounds the run's \
             wall clock, only its wake count",
        ),
        what: "`the 300 rounds took 3.000355515s, past the 3s bound`, then \
               `3.003595533s` on the harness's own re-run, and \
               `ALONE poll_wake_pipe: red again, the same failure both times — the defect is \
               real`. **Still no wake lost, and still the same assertion**: both panics are \
               `poll_wake_pipe.rs:68:5`, the elapsed bound, 0.36 ms and 3.6 ms over, while the \
               lost-wake `assert_eq!` at `:63` passed both times. The job priced its host at \
               `fastest boot 2274 ms against the reference 1320 ms — liveness ceilings paid at \
               1.72x width` over `4 core(s)`, and that factor reached every host-side ceiling \
               and not this one. It red a merge queue on a branch of host-side gates.",
        evidence: "merge-queue `ci` run 33644950006, job 100297692632 (`guest (1)`), headSha \
                   ae974921, 2026-09-02; the shard was otherwise green at 194 passed, 1 failed, \
                   195 total (135.0s), 81 held back for the nightly tier",
        source: "tests/toyos-rust-tests/src/bin/poll_wake_pipe.rs",
        measured: "2026-09-02",
    },
    Red {
        test: "poll_wake_pipe",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 2),
        standing: Standing::Retired(
            "the assertion it fired on is deleted: the guest binary no longer bounds the run's \
             wall clock, only its wake count",
        ),
        what: "the same job re-run, and the same two panics at `poll_wake_pipe.rs:68:5`: \
               `the 300 rounds took 3.006928225s, past the 3s bound`, then `3.003953429s` on the \
               harness's own re-run, `ALONE poll_wake_pipe: red again, the same failure both \
               times — the defect is real`. **A re-run does not clear it**, which is what \
               stopped the branch it was gating from getting a green run at all. Host priced at \
               `fastest boot 1972 ms against the reference 1320 ms — liveness ceilings paid at \
               1.49x width` over `4 core(s)`: a *faster* host than the attempt above and the \
               same overshoot, so this is not the host getting slower either.",
        evidence: "merge-queue `ci` run 33644950006 attempt 2, job 100312837672 (`guest (1)`), \
                   headSha ae974921, 2026-09-02; the shard was otherwise green at 194 passed, \
                   1 failed, 195 total (134.2s), 81 held back for the nightly tier",
        source: "tests/toyos-rust-tests/src/bin/poll_wake_pipe.rs",
        measured: "2026-09-02",
    },
    Red {
        test: "poll_wake_pipe",
        instrument: Instrument::Ci,
        finding: Finding::fires(2, 2),
        standing: Standing::Retired(
            "the assertion it fired on is deleted: the guest binary no longer bounds the run's \
             wall clock, only its wake count",
        ),
        what: "`the 300 rounds took 3.095220897s, past the 3s bound`, then `3.005337855s`, \
               `ALONE poll_wake_pipe: red again, the same failure both times — the defect is \
               real` — **on an unrelated branch and a different shard**. The IOMMU branch \
               touches interrupt remapping and nothing near a pipe, and this is `guest (2)` \
               where the three jobs above are `guest (1)`, so neither the diff nor one shard's \
               machine is what the bound is measuring. Host priced at `fastest boot 3253 ms \
               against the reference 1320 ms — liveness ceilings paid at 2.46x width` over \
               `4 core(s)`, and that factor reaches every host-side ceiling and not this one.",
        evidence: "pull-request `ci` run 33649279837, job 100311933914 (`guest (2)`), branch \
                   `iommu-interrupt-remapping`, headSha 08b88a8a, 2026-09-02; the shard was \
                   otherwise green at 195 passed, 1 failed, 196 total (105.5s), 81 held back \
                   for the nightly tier",
        source: "tests/toyos-rust-tests/src/bin/poll_wake_pipe.rs",
        measured: "2026-09-02",
    },
    // Found auditing the merge-health backfill
    // (`issues/build/the-eased-merge-law-carries-a-threshold.md`), not by
    // anyone working the diff it rode on.
    Red {
        test: "console_locale_detect",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "typing was the loss, and the fix had already landed when this row was read back: \
             the sighting's tree typed `locale detect` with `QmpInput::type_text` — 26 set-1 \
             bytes in one QMP batch against QEMU's 16-byte device queue, unverified — and the \
             job's own capture shows what that costs: the shell echoed `locale dct` and ran it \
             (`locale: no layout named 'dct…'`), the i8042 counter line reads 66 bytes against \
             the 72 injected with the last at 1986ms, and the wizard was never asked for, so \
             the marker wait ran out against a guest idling at a prompt. Not the \
             `desktop_locale_detect` boot race this row filed it beside — the keyboard was \
             never lent because the command that lends it never ran. `shell_type_line` \
             (7a033450, 2026-08-26) is the fix: bursts bounded by the queue, the guest's own \
             echo of the whole line as the verdict, three tries — a lost byte is retyped \
             instead of stalling the marker wait",
        ),
        what: "`STALLED: waiting for the wizard to ask for a key under /bin/console — the \
               console did not lend it the keyboard — it never stopped talking and never got \
               there`. Same shape as `desktop_locale_detect`'s terminal-boot-race family — a \
               wizard waiting for a key it was never handed — but against `/bin/console` rather \
               than `/bin/terminal`, so it is not provably the same race and is not folded into \
               it. First sighting: `--known-red` answered `NOT ON THE LIST`. **Not about the \
               diff it was found on**, which is #142's log-redesign decision record, no kernel \
               byte. `ALONE: GREEN, and it was alone both times — a rate and not a \
               classification`",
        evidence: "push-triggered `ci` run 32314166262, job 96263949273 (`guest (9)`), headSha \
                   eba06ad6, 2026-08-19",
        source: "tests/toyos.rs",
        measured: "2026-08-20",
    },
    // ---------------------------------------------------------------------
    // `wt/toyos-i8042deep`, dev host, 2026-08-19. Adjudicated here rather than
    // re-run away: each answered `NOT ON THE LIST` when it was asked, and the
    // branch they appeared on touches no kernel file at all — its whole delta
    // is `tests/toyos.rs` and this file.
    //
    // Two of the three were `i8042_budget_expiry` and `nvme_large_device`,
    // machine-wide Ring 0 deaths at `0x1b` that reded whichever guest was
    // booting. Their rows are gone with the defect — a `context_switch`
    // restoring a task another CPU was still standing on — so a red under
    // either name now is a new measurement and must be read as one. The one
    // below was read as not a kernel death when it was written; it was the
    // silent one, and it is retired with the same session's two others.
    // ---------------------------------------------------------------------
    Red {
        test: "diskless_boot",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "the same `QEMU died before ===READY=== (status … 0)` as `screen_fatal_halt`'s \
             2026-08-15 row, retired 2026-08-22 for the same reason: under `-no-reboot` a \
             status-0 exit before the marker is a guest that reset itself, which is the \
             silent death of PR #202's class (commit 5e74971e; 37 deaths in 13,960 unfixed \
             twelve-wide boots, 25 of them silent, 0 in 7,418 fixed). Twelve wide with another \
             worktree's suite on the host is the exposure that class predicts, and nothing in \
             the sighting points elsewhere — which is what `Not investigated` below was \
             waiting for",
        ),
        what: "`[qemu] QEMU died before ===READY=== (status: Ok(ExitStatus(unix_wait_status(0))))`. \
               **QEMU exited zero**, so this is neither a panicked guest nor a wall-clock guard \
               reporting the content it meant to assert — the process went away cleanly before \
               the guest was ready, which nothing in the register explains. 7 s under load \
               against 3 s alone; `ALONE: GREEN`. Not investigated",
        evidence: "the same run as this session's `nvme_large_device` row",
        source: "issues/build/parallel-tests-red-under-other-suites.md",
        measured: "2026-08-19",
    },
    // ---------------------------------------------------------------------
    // `wt/toyos-killwrite`, dev host, 2026-08-20, on `e4c2c8ff` — `main`'s own
    // tip, unmodified. The row
    // `issues/kernel/deferred-release-outlives-its-syscall.md` said was owed: the
    // name answered `NOT ON THE LIST` when that file was written, so a landing
    // gate that hit it had nothing to check the red against. Two rows, because
    // one name measured on two instruments in one session is two measurements.
    // ---------------------------------------------------------------------
    Red {
        test: "kill_while_blocked",
        instrument: Instrument::DevHostAlone,
        finding: Finding::fires(2, 53),
        standing: Standing::Stands,
        what: "`a pipe whose only reader was killed mid-read still took a write` (arm 1) once, \
               and `a connection whose peer was killed mid-read still took a write`, \
               `left: Ok(22)` `right: Err(NotFound)` (arm 2) once — **one red on each of the two \
               arms**, which is what says they are one mechanism rather than two paths of \
               different speeds. It is not a classification: `ALONE … GREEN` both times, and \
               `Sched::Serial` would retire it no better than it retired `handle_lifetime`. The \
               release a peer's answer rides runs from `object::drain_zero_handles`, and the \
               killing syscall's own drain site can be robbed of the batch by any other CPU's",
        evidence: "53 × `cargo test --test toyos-build -- kill_while_blocked` in one session, the \
                   host reporting 1.68x–2.70x width throughout; the same session staged the \
                   mechanism at 4 of 5 by removing the syscall-exit drain",
        source: "issues/kernel/deferred-release-outlives-its-syscall.md",
        measured: "2026-08-20",
    },
    Red {
        test: "kill_while_blocked",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::quiet(4),
        standing: Standing::Stands,
        what: "four consecutive full fast tiers, 272 tests each, and it did not fire in any of \
               them. **This retires nothing and is here so that it cannot be read as retiring \
               anything**: the first sighting of this defect was inside a 272-test run, and four \
               runs is not a denominator that reaches a rate of two in fifty-three",
        evidence: "4 × `cargo test --test toyos-build`, same tree and session as this name's \
                   dev-host-alone rate",
        source: "issues/kernel/deferred-release-outlives-its-syscall.md",
        measured: "2026-08-20",
    },
    // ---------------------------------------------------------------------
    // The T14 guest lane, 2026-08-21. `985f3834` moved every trusted event's
    // `guest` job from twelve hosted shards to one 1/1 lane on the T14, and
    // `tests/test-durations` still holds what twelve hosted shards measured. The
    // durations gate compares the two and reds — on `main`'s own tip as readily
    // as on a pull request, and on a different set of names each run.
    //
    // **Not a red about the tree, and the row is here so nobody re-derives
    // that.** `main`'s tip measured 6,845 ms in the profile's own hosted
    // twelve-way shape hours earlier (merge-queue run 32505371471, green, no
    // name over the ceiling), and twenty interleaved reps an arm on the T14
    // cannot tell that tip from the tree the profile was recorded on:
    // p = 0.42, and each arm put 2 of 20 reps over the line.
    // ---------------------------------------------------------------------
    Red {
        test: "xhci_full_speed_device",
        instrument: Instrument::Ci,
        finding: Finding::fires(3, 4),
        standing: Standing::Stands,
        what: "the durations gate and not the test, which passes: `xhci_full_speed_device \
               measured 10166 ms in CI, over the 10000 ms line, but xhci_full_speed_device \
               remains Fast` — 11,076, 12,156, 10,166 and 9,052 ms across four T14 lanes, \
               against a committed 6,900 ms that twelve hosted shards measured and still \
               measure. **The fourth is the point**: it crossed nothing, and the same run \
               reded on `i8042_health` at 15,122 ms instead",
        evidence: "the four consecutive T14 1/1 `guest` lanes of 2026-08-21 — runs 32498159547 \
                   (`main` 07f89c8b, 9 names over the ceiling), 32506479551 (`main` 13953023, \
                   5), 32513441183 (PR #199, 1) and 32524769419 (PR #201, 1, a different name) \
                   — whose lane totals were 548.8 s, 483.6 s, 429.2 s and 444.1 s of tests",
        source: "src/durations.rs",
        measured: "2026-08-21",
    },
    // The same name on the instrument the profile *is* — twelve hosted shards —
    // where it is not a machine gap at all but the test's own spread. Both rows
    // stand: the one above is about a T14 lane pricing a hosted profile, this
    // one is about the hosted lane pricing itself.
    Red {
        test: "xhci_full_speed_device",
        instrument: Instrument::Ci,
        finding: Finding::fires(1, 6),
        standing: Standing::Stands,
        what: "the durations gate and not the test, again, and from the other side of the \
               commitment line: `xhci_full_speed_device is priced at 9890 ms — over the 8000 ms \
               a Fast test may be committed at and under the 10000 ms line — and \
               xhci_full_speed_device remains Fast`. Since 2026-08-22 a landing renders the \
               price verdict only for names it registered or re-tiered, so on a pull request or \
               a merge-queue composition that left this name alone the same sentence prints as \
               a `::warning::` and the job exits 0; **on the nightly it is a red, and it is a \
               finding about this test's variance to be fixed at the test** — not a re-run, and \
               not a `Why::Cost` row, which the return rule refuses the moment a run prices it \
               at or under 8,000 ms",
        evidence: "six hosted twelve-shard runs, 2026-08-20 to 2026-08-22, priced it 4,700, \
                   6,816, 6,900, 7,456, 7,499 and 9,890 ms with its two slowest shards \
                   producing its second- and third-cheapest prices; within-name sd of ln(price) \
                   0.219 against a population 0.124 over 640 observations, the 9th most \
                   variable of 83 Fast names. The 9,890 ms reading dequeued merge-queue \
                   composition 32550410305",
        source: "issues/build/xhci-full-speed-device-jumped-47-percent-over-its-commitment.md",
        measured: "2026-08-22",
    },
    // One observation, so `Seen` and not a rate: the harness re-ran it alone and
    // it passed, and its own verdict line declined to call that a
    // classification. The row is here because a `<symbol unread: …>` stopped
    // being routine weather when `reap_poisoned` stopped taking the process
    // table on every idle trip, so the next reader of this name gets the
    // mechanism instead of `NOT ON THE LIST`.
    //
    // Both rows retire together on 2026-08-22: the crash report no longer asks
    // the process table for a symbol at all, so what they measured cannot be
    // produced. The mechanism, the counts and the negative control are the
    // retirement reason below; a `<symbol unread: …>` under either name now is a
    // new measurement of a different thing, and its text says which.
    Red {
        test: "panic_recovery",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "PR #239: a crash report reads its symbols off the running task's own record, \
             lock-free, so `<symbol unread: the process table was held>` is not a string the \
             kernel can emit. The question these two rows left open was which holder, and the \
             answer is that there is no single one — the table's takers in a conceding window \
             are a spawn, a demand-paged fault and an exit, which is every process in the \
             machine doing ordinary work. **The rate, dev host, twelve-wide suite looping \
             beside it as company, N = 12 rounds of `fault_gates` + `panic_recovery` an arm, \
             2026-08-22:** 3 of 12 rounds conceded a frame before the fix (7 log lines, 3 \
             distinct frames, host width 1.70x-4.78x); 0 of 12 after (1.69x-3.55x); and 1 of 12 \
             with the fix reverted on the *same* base (1.70x-5.84x), which is the negative \
             control — the pre-fix code still concedes on the tree the after arm was green on. \
             Six full 12-wide suites of the fixed tree beside those arms conceded nothing \
             either",
        ),
        what: "`the crash report could not read a symbol it was asked for, so a bare address \
               in it is a lost race and not a verdict: 0x100000072ce <symbol unread: the \
               process table was held>` — one frame of one report, at an address cpu1's own \
               report had resolved by name (`_start+0xe`) three milliseconds earlier, while \
               cpu1 was itself in the panic path",
        evidence: "the T14 1/1 `guest` lane of run 32527751613 (job 96913340222, PR #204, \
                   2026-08-21 21:25Z), whose re-run alone was green",
        source: "tests/toyos.rs",
        measured: "2026-08-21",
    },
    // The same concession under a second name, on the other instrument: a
    // hosted twelve-shard run, not the T14. `fault_gates` spawns children into
    // deliberate faults the way `panic_recovery` spawns them into panics, so two
    // reports overlapping is its ordinary weather too.
    Red {
        test: "fault_gates",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "PR #239, with the row above and on the same evidence: the lookup takes no lock, so \
             neither name can produce this string. Both arms of the rate ran both names, and \
             `fault_gates` supplied 1 of the 3 pre-fix rounds — its concession was the `rip:` \
             line of a #DE report whose backtrace named every frame including the one above it, \
             at 0.418 s against `spawn: /bin/test_rs_fault_gate_child pid=8 … symbols=2048KiB \
             (total=6ms)` at 0.409 s, with that child's own 2,798 us demand-paged instruction \
             fetch inside the window",
        ),
        what: "`the crash report could not read a symbol it was asked for, so a bare address \
               in it is a lost race and not a verdict` — `check_symbols_were_read` reding on a \
               `<symbol unread: the process table was held>` frame, the row above's mechanism \
               under a second name; the process exited 0 and the harness re-ran it alone green",
        evidence: "hosted `guest (1)` of run 32573597349 (job 97032648155, PR #227, 2026-08-22 \
                   12:45Z), a diff of the scheduler simulator and one kernel `Balance` enum \
                   with no crash-path code in it",
        source: "tests/toyos.rs",
        measured: "2026-08-22",
    },
    // ---------------------------------------------------------------------
    // `/log`'s `fsync` under contention. The sighting first, then the rate that
    // reproduced it — two measurements of one defect, on two sessions, and the
    // second is the one that names the mechanism because the failure arm had
    // been dropping the kernel's own lines until `wt/toyos-fsync`.
    // ---------------------------------------------------------------------
    Red {
        test: "esp_filesystem",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "2026-08-23: `SYS_FSYNC` retries a budget-refused flush above every lock \
             (`object/ops.rs`), bounded by `block::DEADMAN`, so this producer no longer \
             reaches userland as an error; `log_flush_retry` stages the refusal and gates \
             the retry, and the row below carries the mechanism",
        ),
        what: "`fsync the blob: Kind(Other)` at `src/bin/esp_files.rs:130:22` — the five checks \
               before it passed, so only the `/log` write path failed and only at the flush. \
               Which layer refused is not in this evidence: the harness reported the guest's \
               stdout and dropped the kernel log, which is where every `log!` on that path lands",
        evidence: "`wt/toyos-returnrule` at 02a087fd, dev host, 2026-08-21, one full `cargo test` \
                   of 79 guests at `fastest boot 2058 ms … 1.56x width`; the harness's own re-run \
                   answered `ALONE esp_filesystem: GREEN` (4 s)",
        source: "tests/common/volumes.rs",
        measured: "2026-08-21",
    },
    Red {
        test: "esp_filesystem",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(1, 73),
        standing: Standing::Retired(
            "2026-08-23: the timeout-dead retry moved to the operation level — a \
             budget-refused flush answers `WouldBlock` and `object/ops.rs`'s fsync loop \
             re-issues it on a fresh budget off the pinned path, `MAX_TRANSPORT_ATTEMPTS` \
             stays for the cheap breaks, and `log_flush_retry`'s first boot reds if the \
             retry stops delivering the refused pages",
        ),
        what: "`fsync the blob: Kind(Other)`, and with the kernel log kept the producer is two \
               2 s deadlines in series: `USB_TIMEOUT_NS` breached on the status phase of a \
               WRITE(10) (`transport broke on SCSI 0x2a: no answer in the status phase in \
               2000 ms`, both endpoints still Running, so a live device), Reset Recovery \
               succeeded in 1 ms, and attempt 2 was then refused unissued because \
               `block::OPERATION` is also 2 s (`SCSI 0x2a not issued: 2000ms`). \
               `MAX_TRANSPORT_ATTEMPTS` is unreachable whenever the break was a timeout. The \
               guest spent `syscall_wall=2108ms` in that one `SYS_FSYNC`. Dated: the same \
               break was absorbed by the retry on CI on 2026-08-13 (`SCSI 0x35 completed on \
               attempt 2`), which `block::OPERATION` made impossible when it landed in \
               5479129d on 2026-08-20, one day before the first sighting",
        evidence: "`wt/toyos-fsync` at 8c0f9526, dev host, 2026-08-22 12:09:11Z-13:09:27Z, 73 \
                   consecutive full 12-wide suites of 272 tests; `wt/toyos-dmapool` shared the \
                   twelve guest slots for 21 of them, the red among those. The red's own pass \
                   measured `1.05x width` — the loop's median — so the aggregate width does not \
                   predict it; `ALONE esp_filesystem: GREEN` again",
        source: "tests/common/volumes.rs",
        measured: "2026-08-22",
    },
    // A boot that had not reached logd's first write inside the test's window,
    // once in four beside another agent's suite, green alone and three of three
    // after — no rate yet, and the row exists so the next reader of this name
    // gets the sighting instead of `NOT ON THE LIST`.
    Red {
        test: "kernel_log_file",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`logd never opened a file` — the captured boot log ends at `log-volume: \
               partition mounted` at 0.221 s, so the guest had not reached logd's first write \
               inside the window, not a file written wrong; `ALONE kernel_log_file: GREEN`, \
               which is a hypothesis about its `Sched` and not a finding",
        evidence: "dev host, 2026-08-22, `--nightly kernel_log_file` on merged `main` 11cc6ef1 \
                   at `1.55x width` with a second agent's `cargo test --workspace` on the same \
                   laptop: 1 red, then 3 of 3 green at load 3.1 with the neighbour still present",
        source: "issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md",
        measured: "2026-08-22",
    },
    // ---------------------------------------------------------------------
    // One same-session A/B, six full suites an arm, interleaved on one dev
    // host: `wt/toyos-freeze` (the scheduler's staleness rule) against
    // `origin/main`. Three rows because three names fired and no two of them
    // are one measurement — and the `main` row is here for the reason the
    // branch rows are, because an index that carries only the arm under
    // suspicion is an index that cannot answer whether the arm is the cause.
    // ---------------------------------------------------------------------
    Red {
        test: "fat_backing_revoked",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::fires(3, 6),
        standing: Standing::Disputed(
            "the arms do not separate at this sample size, and nobody may read this row either \
             way. 3 of 6 against 0 of 6 on one name is p ~ 0.18 by Fisher's exact test, and the \
             *class* — a volume checker complaining after a loaded run — fired in both arms, 4 of \
             6 on the branch and 1 of 6 on `main`. The coupling is real and is named rather than \
             denied: the branch refuses a CPU whose doorbell edge has stood longer than a pass may \
             take, and at boot several programs are spawned before any CPU has run one, so a \
             two-CPU guest's boot burst spreads differently and a verdict that depends on when \
             `iod` drains relative to an unlink can change phase on that alone. What retires or \
             confirms it is the same A/B on a quiet host: six runs an arm or more. The six that \
             were started to get it were abandoned — the first took 417.4 s against the 58-79 s of \
             every run above it and `pgrep` found three other worktrees' suites on the box, which \
             `tests/CLAUDE.md` says is discarded and never corrected",
        ),
        what: "`the unlink-and-reallocate cycle left the log volume breaking the format: 1 \
               cluster(s) from 20 are marked allocated and no directory entry reaches them` — one \
               leaked cluster on `/log`, found by `toyos-fat32-check` after the guest had shut \
               down. `ALONE fat_backing_revoked: GREEN` every time",
        evidence: "six full `cargo test` runs on `wt/toyos-freeze` at 8a7b82ee, 58-79 s each, \
                   interleaved in one session with six on `origin/main` at 16c05999",
        source: "issues/build/a-loaded-suite-reds-a-volume-checker-on-both-arms.md",
        measured: "2026-08-27",
    },
    Red {
        test: "device_claim_lifetime",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Disputed(
            "one sighting in the same six-suite arm as the `fat_backing_revoked` row above, in the \
             one run that produced two reds, and a single sample is no rate. It is recorded apart \
             from that row because two guests failing in one phase is not by itself a claim about \
             a common cause — `Instrument::DevHostLoaded`'s own doc carries the arithmetic — and \
             folding them into one measurement would have invented the claim it declines to make",
        ),
        what: "`exit code Some(101)` from its guest binary, with \
               `exit: test_rs_device_claim_lifeti pid=62 code=101 cpu=136ms` on the wire; \
               `ALONE device_claim_lifetime: GREEN`",
        evidence: "one of six full `cargo test` runs on `wt/toyos-freeze` at 8a7b82ee, the same \
                   run that reddened `fat_backing_revoked`",
        source: "issues/build/a-loaded-suite-reds-a-volume-checker-on-both-arms.md",
        measured: "2026-08-27",
    },
    Red {
        test: "esp_filesystem",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Disputed(
            "**this one is `main`'s**, and it is what stops the two rows above being read as a \
             regression on sight: the third name of one shape — a volume checker complaining after \
             a loaded run — fired on the unmodified tree in the same session, on the same \
             instrument, in six runs. One sighting, so no rate; recorded because a row measured \
             only on the arm under suspicion cannot answer whether the arm is the cause",
        ),
        what: "red in the wide phase, `ALONE esp_filesystem: GREEN`",
        evidence: "one of six full `cargo test` runs on `origin/main` at 16c05999, 58-79 s each, \
                   interleaved in one session with six on `wt/toyos-freeze`",
        source: "issues/build/a-loaded-suite-reds-a-volume-checker-on-both-arms.md",
        measured: "2026-08-27",
    },
    // ---------------------------------------------------------------------
    // Two `main`-push `ci` runs on 2026-08-28, both red in the `durations`
    // job and nowhere else: the shard that ran the name passed it.
    // ---------------------------------------------------------------------
    Red {
        test: "log_conservation_smp4",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "relegated Why::Cost to Nightly in this landing — the straddling four-CPU width \
             leaves the per-PR durations gate, and smp1/smp8 keep the conservation law at both \
             subject shapes",
        ),
        what: "not the test — its **price**: `log_conservation_smp4 is priced at 8248 ms — over \
               the 8000 ms a Fast test may be committed at and under the 10000 ms line — and \
               log_conservation_smp4 remains Fast: priced without margin, so relegate it or make \
               it faster. A price this close to the line is decided by which partition ran it, \
               and reds whichever pull request measures it next`. The gate's own sentence is the \
               diagnosis: a straddler, the 2026-08-21 class `src/tiers.rs` documents, against a \
               committed 5512 ms in `tests/test-durations`. `src/tiers.rs`'s law names the two \
               exits — relegate it or make it faster — and its siblings say which: \
               `log_conservation_smp1` and `log_conservation_smp8` are priced 4686 and 5112 with \
               margin, so a `Why::Cost` relegation of the middle width alone keeps the log's \
               conservation law per-pull-request at both subject shapes (producer sharing the \
               reader's CPU, and not). Recorded rather than relegated in the same landing: a \
               tier move is a coverage decision, left to its owner with this row as the case",
        evidence: "`ci` runs 33202812787 (8572 ms) and 33212528174 (8248 ms), both `main` pushes \
                   on 2026-08-28, each red only in `durations` and its aggregate; `guest (12)` \
                   in the second passed the test itself in 8 s",
        source: "tests/common/logread.rs",
        measured: "2026-08-28",
    },
    // ---------------------------------------------------------------------
    // This branch's own composition run: the tier batch put the whole nightly
    // set on one PR's shards, and a fresh test's fixed drain window met a
    // loaded one.
    // ---------------------------------------------------------------------
    Red {
        test: "sysret_ss_reload",
        instrument: Instrument::Ci,
        finding: Finding::Seen,
        standing: Standing::Retired(
            "this same landing: the probe line is waited for on a 10 s liveness ceiling \
             (`drain_until`) instead of a fixed 500 ms drain, so iod running late costs \
             patience, not the verdict",
        ),
        what: "`the SS-reload probe never reported — iod may not have run it` on the wide \
               shard, then ALONE green with `the switch reloads SS from null before a \
               sysretq can see it` — the harness's own verdict: `it failed once and passed \
               once. That is a rate and not a classification`. The reload was never in \
               question; the 500 ms fixed drain after boot was shorter than a loaded \
               shard's path to iod's probe",
        evidence: "`ci` run 33246638742, `guest (12)`, 2026-08-29, red then alone-green in \
                   the same job",
        source: "tests/toyos.rs",
        measured: "2026-08-29",
    },
    // The name has a second red mode, and the write-up that owned it described
    // only the first: a 192 s liveness ceiling on an `smp: 2` guest. This is an
    // assertion at 4 s, and nobody has a mechanism for it.
    Red {
        test: "launcher_refusals",
        instrument: Instrument::DevHostLoaded,
        finding: Finding::Seen,
        standing: Standing::Stands,
        what: "`launcher_refusals exited Some(101)` on the assertion, not on a ceiling: `16 \
               more refused launches left more live objects behind: [(\"PipeWrite\", 0, 1), \
               (\"Connection\", 0, 1)] … init is keeping the handles a refusal took`, and \
               byte-identical on both occurrences",
        evidence: "two whole-suite runs of `iommu-domains` on 2026-09-03, wide phase, beside \
                   two other worktrees' suites: 303/304 in 586 s and 303/304 in 513 s, the \
                   test red at 4 s and 5 s. `ALONE: GREEN` from the harness both times and \
                   green again re-run by hand (3 s, 2 s); the same branch ran 304/304 twice. \
                   The second red mode is written up in \
                   `issues/build/parallel-tests-red-under-other-suites.md`",
        source: "tests/toyos.rs",
        measured: "2026-09-03",
    },
    // The judge read the exit accounting line off a capture that closes at the
    // guest runner's exit report, and nothing waited for it.
    Red {
        test: "syscall_cost",
        instrument: Instrument::Ci,
        finding: Finding::fires(3, 5),
        standing: Standing::Stands,
        what: "the run claims 180000 SYS_CLOCK transitions and the kernel counted 0 — no \
               `syscalls: pid=` line for the process reached the capture",
        // Every hosted run of the name there has been; the three reds are this
        // branch's, each red twice and ALONE in its own job.
        evidence: "red: `ci` 33727591910, 33731006452, 33733759354, `guest`, #380, 2026-09-03. \
                   green: `ci` 33701834606 (main, the push landing #373) `[syscall] 658 cycles \
                   per SYS_CLOCK over 277969 of them`; `ci` 33728852421 (schedule, ubuntu-24) \
                   `563 cycles … over 286777`. The wait is built on this branch and has no \
                   hosted run yet, so this stands until one is green. \
                   issues/build/syscall-cost-reads-the-exit-line-off-a-capture-that-can-close-first.md",
        source: "tests/toyos.rs",
        measured: "2026-09-03",
    },
];

// ---------------------------------------------------------------------------
// The query.
// ---------------------------------------------------------------------------

/// `cargo run -- --known-red [<test>]`.
pub fn dispatch(root: &Path, args: &[String]) {
    let asked = args
        .iter()
        .position(|a| a == "--known-red")
        .and_then(|at| args.get(at + 1))
        .filter(|a| !a.starts_with("--"));
    let registry = Registry::read(root);
    print!("{}", answer(KNOWN_RED, &registry, Day::today(), asked.map(String::as_str)));
}

/// The whole answer, as text, so that the shape of it is a value a test can
/// assert on rather than something only a human ever sees.
fn answer(rows: &[Red], registry: &Registry, today: Day, asked: Option<&str>) -> String {
    match asked {
        Some(test) => one(rows, registry, today, test),
        None => everything(rows, today),
    }
}

/// What the index says about one name, and it is a sentence before it is a list.
#[derive(PartialEq, Eq, Debug)]
enum Verdict {
    /// A measurement says it reds and nothing has retired that measurement.
    KnownRed,
    /// No live red, but the sources disagree about whether one was retired.
    Disputed,
    /// Rows exist and none of them is a live red.
    NotKnownRed,
    /// No rows. **Not** a claim that the test is green.
    NotOnTheList,
}

fn verdict_for(mine: &[&Red]) -> Verdict {
    if mine.is_empty() {
        return Verdict::NotOnTheList;
    }
    if mine.iter().any(|r| r.finding.is_red() && r.standing == Standing::Stands) {
        return Verdict::KnownRed;
    }
    if mine.iter().any(|r| matches!(r.standing, Standing::Disputed(_))) {
        return Verdict::Disputed;
    }
    Verdict::NotKnownRed
}

fn headline(v: &Verdict) -> &'static str {
    match v {
        Verdict::KnownRed => "KNOWN-RED",
        Verdict::Disputed => "DISPUTED",
        Verdict::NotKnownRed => "NOT KNOWN-RED",
        Verdict::NotOnTheList => "NOT ON THE LIST",
    }
}

fn rows_for<'a>(rows: &'a [Red], test: &str) -> Vec<&'a Red> {
    let mut mine: Vec<&Red> = rows.iter().filter(|r| r.test == test).collect();
    // Newest first: what was measured last is what a reader wants at the top,
    // and the day is printed beside every row so the order is checkable.
    mine.sort_by_key(|r| std::cmp::Reverse(Day::parse(r.measured)));
    mine
}

fn one(rows: &[Red], registry: &Registry, today: Day, test: &str) -> String {
    let mine = rows_for(rows, test);
    let verdict = verdict_for(&mine);
    let mut out = format!("{test}: {}\n", headline(&verdict));

    if verdict == Verdict::NotOnTheList {
        out += if registry.tests.contains(test) {
            "\n  No measurement in this index has ever named it. That is not a claim that it is\n  \
             green — it is a claim that nobody wrote down a rate for it.\n"
        } else {
            "\n  No test of that name is registered either, so this is a typo or a renamed test.\n  \
             `cargo test -- --list` is the registry.\n"
        };
        return out;
    }

    if verdict == Verdict::KnownRed {
        out += "\n  At least one measurement says it reds and nothing has retired that\n  \
                measurement. Read which instrument each row is about before acting on it.\n";
    }

    let mut instruments: BTreeSet<Instrument> = BTreeSet::new();
    for r in &mine {
        instruments.insert(r.instrument);
        let standing = match r.standing {
            Standing::Stands => String::new(),
            Standing::Retired(why) => wrapped("RETIRED   ", why),
            Standing::Disputed(how) => wrapped("DISPUTED  ", how),
        };
        out += &format!(
            "\n  {:<13}  {:<16}  {}, {}{}\n{}{standing}{}{}",
            r.finding.rendered(),
            r.instrument.label(),
            r.measured,
            age(r, today),
            expiry_note(r, today),
            wrapped("", r.what),
            wrapped("evidence  ", r.evidence),
            wrapped("write-up  ", r.source),
        );
    }

    out += "\n  What each instrument cannot say:\n";
    for i in instruments {
        out += &wrapped(&format!("{:<16}  ", i.label()), i.cannot_say());
    }

    if registry.exempted.contains(test) {
        out += "\n  ALSO DECLARED in EXPECTED_FAILURES (`tests/toyos.rs`), which is a different\n  \
                mechanism: a named red with a task and a write-up that makes the run exit 0, for\n  \
                one quoted assertion only. Nothing in this index exempts anything.\n";
    }
    out
}

/// How long ago the measurement was taken, which is half of what a row is worth.
fn age(r: &Red, today: Day) -> String {
    match Day::parse(r.measured).map(|d| d.until(today)) {
        None => "and that date does not parse".to_string(),
        Some(0) => "today".to_string(),
        Some(1) => "1 day ago".to_string(),
        Some(n) => format!("{n} days ago"),
    }
}

/// One field of a row, indented under it and folded so that a paragraph is
/// readable in a terminal. The whole point of the file is that somebody reads
/// the answer instead of grepping the prose.
fn wrapped(field: &str, text: &str) -> String {
    const INDENT: &str = "      ";
    const WIDTH: usize = 86;
    let mut out = String::from(INDENT) + field;
    let mut column = INDENT.len() + field.len();
    let hang = " ".repeat(column);
    for (n, word) in text.split_whitespace().enumerate() {
        if n > 0 && column + 1 + word.len() > WIDTH {
            out += "\n";
            out += &hang;
            column = hang.len();
        } else if n > 0 {
            out += " ";
            column += 1;
        }
        out += word;
        column += word.len();
    }
    out + "\n"
}

fn expiry_note(r: &Red, today: Day) -> String {
    if r.standing != Standing::Stands {
        return String::new();
    }
    let Some(due) = Day::parse(r.measured).map(|d| d.plus_days(SHELF_LIFE_DAYS)) else {
        return String::new();
    };
    let left = today.until(due);
    if left <= 0 {
        "  ** EXPIRED: nobody has measured this since **".to_string()
    } else if left <= 7 {
        format!("  ** expires in {left} days **")
    } else {
        String::new()
    }
}

/// The index's headline counts, arrived at in one place so that the answer a
/// reader gets and the gate over it read the same arithmetic. `live` and
/// `expiring` are both counted among the standing rows alone.
#[derive(PartialEq, Eq, Debug)]
pub struct Census {
    pub rows: usize,
    pub standing: usize,
    pub live: usize,
    pub expiring: usize,
}

impl Census {
    pub fn of(rows: &[Red], today: Day) -> Census {
        let standing = || rows.iter().filter(|r| r.standing == Standing::Stands);
        Census {
            rows: rows.len(),
            standing: standing().count(),
            live: standing().filter(|r| r.finding.is_red()).count(),
            expiring: standing()
                .filter(|r| {
                    Day::parse(r.measured)
                        .is_some_and(|d| today.until(d.plus_days(SHELF_LIFE_DAYS)) <= 7)
                })
                .count(),
        }
    }

    fn rendered(&self) -> String {
        format!(
            "{} standing, {} of them live reds, {} expiring within 7 days.",
            self.standing, self.live, self.expiring
        )
    }
}

fn everything(rows: &[Red], today: Day) -> String {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for r in rows {
        names.insert(r.test);
    }
    let mut out = format!(
        "{} measurements of {} tests. {} `--known-red <test>` for the rows.\n\n",
        rows.len(),
        names.len(),
        Census::of(rows, today).rendered(),
    );
    for test in &names {
        let mine = rows_for(rows, test);
        let v = verdict_for(&mine);
        let newest = mine.first().map_or("", |r| r.measured);
        let live: Vec<String> = mine
            .iter()
            .filter(|r| r.standing == Standing::Stands && r.finding.is_red())
            .map(|r| format!("{} on {}", r.finding.rendered(), r.instrument.label()))
            .collect();
        let line =
            format!("  {:<30}  {:<16}  newest {newest}  {}", test, headline(&v), live.join("; "));
        out += line.trim_end();
        out += "\n";
    }
    let oldest = rows
        .iter()
        .filter(|r| r.standing == Standing::Stands)
        .filter_map(|r| Day::parse(r.measured).map(|d| (d.until(today), r.measured)))
        .max();
    if let Some((age, day)) = oldest {
        out += &format!(
            "\nThe oldest standing measurement is {day}, {age} days old; a standing row expires \
             {SHELF_LIFE_DAYS} days after it was taken.\n"
        );
    }
    out
}

// ---------------------------------------------------------------------------
// What the tree itself says, which is what the rows are checked against.
// ---------------------------------------------------------------------------

/// Every name the suite can produce a verdict for, and the names
/// `EXPECTED_FAILURES` declares — read out of the harness rather than restated,
/// because a restatement is the thing that drifts.
pub struct Registry {
    pub tests: BTreeSet<String>,
    pub exempted: BTreeSet<String>,
}

impl Registry {
    pub fn read(root: &Path) -> Registry {
        let harness = std::fs::read_to_string(root.join("tests/toyos.rs"))
            .expect("tests/toyos.rs is what registers a test name");
        let mut tests = BTreeSet::new();
        // `MACHINE_TESTS` and `SCREEN_TESTS`: `("name", Sched::…)`, a shape
        // nothing else in that file has.
        for (at, _) in harness.match_indices("\", Sched::") {
            let head = &harness[..at];
            if let Some(open) = head.rfind("(\"") {
                let name = &head[open + 2..];
                if !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
                    tests.insert(name.to_string());
                }
            }
        }
        // `AUDIO_TESTS`, whose tuples carry the same explicit Tier as the
        // machine and screen registries. Read the value block rather than a
        // line: formatting it across lines must not make known-red rows stale.
        if let Some(at) = harness.find("const AUDIO_TESTS:") {
            let block = &harness[at..];
            let end = block.find("];").unwrap_or(block.len());
            for tuple in block[..end].split("(\"").skip(1) {
                if let Some(name) = tuple.split('"').next() {
                    tests.insert(name.to_string());
                }
            }
        }
        // The shared boot's binaries are discovered from what is built, and the
        // sources are what they are built from.
        for (dir, ext) in
            [("tests/toyos-rust-tests/src/bin", "rs"), ("tests/testcases/tinycc", "c")]
        {
            let path = root.join(dir);
            for entry in std::fs::read_dir(&path)
                .unwrap_or_else(|e| panic!("{} is where a guest test comes from: {e}", path.display()))
                .flatten()
            {
                let p = entry.path();
                if p.extension().is_some_and(|e| e == ext) {
                    tests.insert(p.file_stem().unwrap().to_string_lossy().into_owned());
                }
            }
        }

        let mut exempted = BTreeSet::new();
        if let Some(at) = harness.find("const EXPECTED_FAILURES: &[ExpectedFailure] = &[") {
            let block = &harness[at..];
            let end = block.find("\n}];").map_or(block.len(), |e| e + 4);
            for piece in block[..end].split("test: \"").skip(1) {
                if let Some(name) = piece.split('"').next() {
                    exempted.insert(name.to_string());
                }
            }
        }
        Registry { tests, exempted }
    }
}

/// Everything a row has to be able to say about itself, against the tree.
///
/// A function over a slice rather than over [`KNOWN_RED`], because a
/// well-formed index cannot exercise a rejection and a gate nobody has watched
/// refuse anything is a gate nobody has watched.
#[cfg(test)]
fn refusals(rows: &[Red], registry: &Registry, root: &Path, today: Day) -> Vec<String> {
    let mut bad = Vec::new();
    let mut seen: BTreeSet<(&str, Instrument, &str)> = BTreeSet::new();

    for r in rows {
        let at = format!("{} ({})", r.test, r.evidence);

        if !registry.tests.contains(r.test) {
            bad.push(format!(
                "{at}: no list registers `{}` — a renamed or deleted test takes its rows with it, \
                 or the index is answering about whatever gets that name next",
                r.test
            ));
        }
        for (field, value) in
            [("what", r.what), ("evidence", r.evidence), ("source", r.source), ("measured", r.measured)]
        {
            if value.trim().is_empty() {
                bad.push(format!("{at}: `{field}` is empty, and there is no default"));
            }
        }
        if let Standing::Retired(why) | Standing::Disputed(why) = r.standing {
            if why.trim().is_empty() {
                bad.push(format!("{at}: a row that is not standing has to say what did that"));
            }
        }
        if let Finding::Fires { red, of } = r.finding {
            // `Finding::fires` refuses these at compile time; a hand-built
            // `Finding::Fires { .. }` is what this catches.
            if red == 0 || of < 2 || red > of {
                bad.push(format!("{at}: {red} of {of} is not a rate"));
            }
        }
        if let Finding::Quiet { of } = r.finding {
            if of < 2 {
                bad.push(format!("{at}: 0 of {of} is one sample and retires nothing"));
            }
        }

        let path = r.source.split_whitespace().next().unwrap_or("");
        let full = root.join(path);
        match std::fs::read_to_string(&full) {
            Err(_) => bad.push(format!(
                "{at}: its write-up `{path}` does not resolve, and an evidence pointer that misses \
                 reads as checked"
            )),
            Ok(text) => {
                if !text.contains(r.test) {
                    bad.push(format!(
                        "{at}: `{path}` never names `{}`, so the row and the prose behind it have \
                         drifted apart",
                        r.test
                    ));
                }
            }
        }

        match Day::parse(r.measured) {
            None => bad.push(format!(
                "{at}: `measured: {}` is not a YYYY-MM-DD date, so the row would never expire",
                r.measured
            )),
            Some(day) => {
                if day > today {
                    bad.push(format!(
                        "{at}: `measured: {}` is in the future, which is a fuse set forward",
                        r.measured
                    ));
                } else if r.standing == Standing::Stands
                    && today >= day.plus_days(SHELF_LIFE_DAYS)
                {
                    bad.push(format!(
                        "{at}: measured {}, more than {SHELF_LIFE_DAYS} days ago, and still \
                         standing. It says nothing about whether the defect is there — it says \
                         nobody has measured since. Re-take it, retire it with what retired it, \
                         or **delete it**: a rate nobody will re-measure is not something anyone \
                         should be trusting",
                        r.measured
                    ));
                }
            }
        }

        if !seen.insert((r.test, r.instrument, r.evidence)) {
            bad.push(format!(
                "{at}: the same test, instrument and evidence twice — one measurement is one row"
            ));
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn registry() -> Registry {
        Registry::read(&repo_root())
    }

    /// The registry is what every other gate here is measured against, so a
    /// parse that quietly found nothing would wave the whole index through.
    #[test]
    fn the_registry_is_read_out_of_the_harness_and_is_not_empty() {
        let r = registry();
        for name in [
            "desktop_window_child",
            "hda_tone",
            "screen_pager_keys",
            "audio_tone",
            "audio_tone_load",
        ] {
            assert!(r.tests.contains(name), "the test-name scan missed `{name}`");
        }
        for name in ["std_unwind", "fpu_isolation"] {
            assert!(r.tests.contains(name), "the guest-binary scan missed `{name}`");
        }
        assert!(r.tests.len() > 100, "only {} test names found", r.tests.len());
        // CLAUDE.md: two expected failures stand, and neither may be
        // reclassified or deleted. This index is not allowed to be the reason
        // nobody notices one going.
        for name in ["desktop_window_child", "hda_tone"] {
            assert!(
                r.exempted.contains(name),
                "`{name}` is no longer in EXPECTED_FAILURES; the root CLAUDE.md says both entries \
                 stand, so either that rule changed or this parse did"
            );
        }
    }

    #[test]
    fn every_row_can_say_what_it_claims() {
        let bad = refusals(KNOWN_RED, &registry(), &repo_root(), Day::today());
        assert!(
            bad.is_empty(),
            "the known-red index is what `--known-red` answers from:\n  {}",
            bad.join("\n  ")
        );
    }

    /// The arithmetic behind the three numbers the answer opens with, against a
    /// table small enough to count by hand: the same filters re-typed over
    /// `KNOWN_RED` would agree with themselves whatever they said.
    #[test]
    fn the_index_counts_what_it_is_carrying() {
        let today = Day::parse("2026-08-11").unwrap();
        let row = Red {
            test: "a_real_test",
            instrument: Instrument::Ci,
            finding: Finding::fires(1, 5),
            standing: Standing::Stands,
            what: "x",
            evidence: "run 1",
            source: "src/redlist.rs",
            measured: "2026-08-10",
        };
        // A standing row expires SHELF_LIFE_DAYS after it was measured, so
        // "expiring within 7 days" reaches 24 days back from `today`.
        let fixture = [
            Red { ..row },
            Red { finding: Finding::Seen, measured: "2026-07-18", ..row },
            Red { finding: Finding::quiet(5), measured: "2026-07-01", ..row },
            Red { standing: Standing::Retired("a fix"), ..row },
            Red { standing: Standing::Disputed("two sources"), finding: Finding::Seen, ..row },
            Red { finding: Finding::fires(3, 5), measured: "no date at all", ..row },
        ];
        let census = Census::of(&fixture, today);
        assert_eq!(census, Census { rows: 6, standing: 4, live: 3, expiring: 2 });
        // The sentence too, and not only the struct: the three numbers reach a
        // reader through it, and nothing else would notice two of them swapped.
        assert_eq!(
            census.rendered(),
            "4 standing, 3 of them live reds, 2 expiring within 7 days."
        );

        let live = Census::of(KNOWN_RED, Day::today());
        println!("known-red index: {} rows, {}", live.rows, live.rendered());
    }

    /// The four things the gate exists to refuse, run rather than argued. The
    /// rate rules are absent because they are compile errors:
    /// `Finding::fires(0, 5)` does not build.
    #[test]
    fn the_gate_refuses_what_it_is_written_against() {
        let root = repo_root();
        let today = Day::parse("2026-08-11").unwrap();
        let reg = Registry { tests: ["a_real_test".into()].into(), exempted: BTreeSet::new() };
        let ok = Red {
            test: "a_real_test",
            instrument: Instrument::Ci,
            finding: Finding::fires(1, 5),
            standing: Standing::Stands,
            what: "x",
            evidence: "run 1",
            source: "src/redlist.rs",
            measured: "2026-08-10",
        };
        assert!(refusals(&[ok], &reg, &root, today).is_empty(), "a well-formed row is not refused");

        let cases: [(&str, Red, &str); 6] = [
            (
                "a name no list registers",
                Red { test: "gone_away", ..ok },
                "no list registers",
            ),
            (
                "a write-up that does not resolve",
                Red { source: "nowhere-at-all.md", ..ok },
                "does not resolve",
            ),
            (
                "a write-up that has stopped being about this test",
                Red { source: "Cargo.toml", ..ok },
                "never names",
            ),
            (
                "a date nothing can read",
                Red { measured: "last Tuesday", ..ok },
                "never expire",
            ),
            (
                "a measurement older than its shelf life, still standing",
                Red { measured: "2026-01-01", ..ok },
                "nobody has measured since",
            ),
            (
                "a hand-built rate the constructor would have refused",
                Red { finding: Finding::Fires { red: 0, of: 5 }, ..ok },
                "is not a rate",
            ),
        ];
        for (what, row, says) in cases {
            let bad = refusals(&[row], &reg, &root, today);
            assert!(
                bad.iter().any(|b| b.contains(says)),
                "{what}: expected a refusal naming {says:?}, got {bad:?}"
            );
        }

        // An expired row that has been *retired* is history and does not red:
        // only a standing claim has a shelf life.
        let old_and_retired = Red {
            measured: "2026-01-01",
            standing: Standing::Retired("something landed"),
            ..ok
        };
        assert!(refusals(&[old_and_retired], &reg, &root, today).is_empty());
    }

    /// The distinction the owner got wrong, asked of the answer rather than of
    /// the data: a name measured and found quiet may not read as a red, and a
    /// name nothing has measured may not read as either.
    #[test]
    fn a_zero_never_reads_as_a_red() {
        let root = repo_root();
        let today = Day::parse("2026-08-11").unwrap();
        let reg = Registry {
            tests: ["came_off".into(), "still_reds".into(), "unmeasured".into()].into(),
            exempted: BTreeSet::new(),
        };
        let base = Red {
            test: "came_off",
            instrument: Instrument::Ci,
            finding: Finding::quiet(5),
            standing: Standing::Stands,
            what: "0 of 5 in the probe",
            evidence: "run 1",
            source: "src/redlist.rs",
            measured: "2026-08-10",
        };
        let rows = [
            Red { ..base },
            Red { test: "still_reds", finding: Finding::fires(2, 5), what: "2 of 5", ..base },
        ];
        assert!(refusals(&rows, &reg, &root, today).is_empty());

        let came_off = answer(&rows, &reg, today, Some("came_off"));
        assert!(came_off.starts_with("came_off: NOT KNOWN-RED"), "{came_off}");
        assert!(came_off.contains("QUIET 0 of 5"), "{came_off}");
        assert!(!came_off.contains(": KNOWN-RED"), "{came_off}");

        let reds = answer(&rows, &reg, today, Some("still_reds"));
        assert!(reds.starts_with("still_reds: KNOWN-RED"), "{reds}");

        let never = answer(&rows, &reg, today, Some("unmeasured"));
        assert!(never.starts_with("unmeasured: NOT ON THE LIST"), "{never}");
        assert!(never.contains("not a claim that it is\n  green"), "{never}");

        let typo = answer(&rows, &reg, today, Some("no_such_test"));
        assert!(typo.contains("No test of that name is registered"), "{typo}");
    }

    /// A retired measurement is history and must not silence the live one
    /// beside it, nor be counted as one.
    #[test]
    fn retired_and_disputed_rows_do_not_decide_a_name_by_themselves() {
        let today = Day::parse("2026-08-11").unwrap();
        let reg = Registry {
            tests: ["t".into(), "d".into()].into(),
            exempted: BTreeSet::new(),
        };
        let base = Red {
            test: "t",
            instrument: Instrument::Ci,
            finding: Finding::fires(5, 5),
            standing: Standing::Retired("a fix landed"),
            what: "x",
            evidence: "run 1",
            source: "src/redlist.rs",
            measured: "2026-08-10",
        };
        let only_retired = [Red { ..base }];
        assert!(answer(&only_retired, &reg, today, Some("t")).starts_with("t: NOT KNOWN-RED"));

        let retired_and_live =
            [Red { ..base }, Red { evidence: "run 2", standing: Standing::Stands, ..base }];
        assert!(answer(&retired_and_live, &reg, today, Some("t")).starts_with("t: KNOWN-RED"));

        let disputed = [Red { test: "d", standing: Standing::Disputed("two sources"), ..base }];
        assert!(answer(&disputed, &reg, today, Some("d")).starts_with("d: DISPUTED"));
    }

    /// The one property `--known-red` has that prose does not: the answer says
    /// which machine it is about, and what that machine cannot be asked.
    #[test]
    fn every_answer_names_its_instrument_and_what_that_instrument_cannot_say() {
        let out = answer(KNOWN_RED, &registry(), Day::today(), Some("screen_pager_keys"));
        // Folded for a terminal, so the assertion is against the words and not
        // against where the fold happened to land.
        let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("QUIET 0 of 5 CI"), "{out}");
        assert!(flat.contains("FIRES 3 of 3 dev host, alone"), "{out}");
        assert!(flat.contains("nothing here is about contention"), "{out}");
        assert!(flat.contains("which vendor's reading of an instruction"), "{out}");
    }

    /// `hda_tone` is the one name both mechanisms carry, at two different
    /// assertions, and the answer has to say so or somebody will read the
    /// exemption as covering the row.
    #[test]
    fn a_name_that_is_also_exempted_says_so() {
        let out = answer(KNOWN_RED, &registry(), Day::today(), Some("hda_tone"));
        assert!(out.contains("ALSO DECLARED in EXPECTED_FAILURES"), "{out}");
        assert!(out.contains("Nothing in this index exempts anything"), "{out}");
    }
}
