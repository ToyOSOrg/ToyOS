//! The metal harness's rules over planted readbacks, each written as the loop
//! writes one and read as a run reads one.

use super::*;
use metal::{Metal, Readback};
use toyos_metal::testargs::MetalMode;
use toyos_metal::{
    verdict_file, BootFile, Phases, Refusal, READBACK_BOOT, READBACK_KERNEL, READBACK_LOADER,
    READBACK_VERDICT,
};
use toyos_metal::timings::{Machine, Record};

/// Each bench's first kernel record, as its own boots write it: the AMD
/// bench's before the log's clock is set, which the T14's is given here too.
pub(super) const ON_T14: &str = "[---------- -------- --.--- cpu0 kernel] microcode: cpu0 Intel signature 0x806c1 platform 7 found 0xbe current 0xbe running 0xbe\n";
const ON_AMD: &str = "[---------- -------- --.--- cpu0 kernel] microcode: cpu0 Amd signature 0x860f01 found 0x8600106 loaded 0x860010d running 0x860010d\n";

const BOOTED: &str = "[2026-09-29 18:22:39  1.165 cpu0 kernel] Boot: complete (1165ms)\n";

fn t14() -> Machine {
    Machine {
        vendor: "LENOVO".to_string(),
        product: "20W0003AMZ".to_string(),
        bios: "N34ET71W (1.71 )".to_string(),
    }
}

/// A root holding the T14's record as `toyos-metal --record` writes
/// one, with no number: what every run the loop made had.
fn recorded() -> toyos_tmpdir::TempDir {
    let root = toyos_tmpdir::TempDir::new("metal-records");
    Record::new(&t14()).save(&root).expect("a recorded machine");
    root
}

/// **The loop boots no machine without a record**, so readbacks naming one
/// are judged red, and the judge writes no record for it.
pub fn readbacks_of_a_machine_with_no_record_are_red_and_record_nothing() {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = toyos_tmpdir::TempDir::new("metal-records");
    plant(&dir, "passing", BOOTED, None);
    let tests = [("passes", &PASSING)];
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    assert!(
        metal::judge_readbacks(&root, Bench::T14, &read(&dir, &["passing"]), &runs, &[]),
        "readbacks of an unrecorded machine judged green"
    );
    assert_eq!(Record::load(&root, &t14()), Ok(None));
    // The same readbacks against the record the loop needed are green.
    assert!(!metal::judge_readbacks(&recorded(), Bench::T14, &read(&dir, &["passing"]), &runs, &[]));
}

/// **A run's machine reaches `toyos-metal` as its own `--host`**, in the words
/// a driven run spawns and a staged run's `request.txt` lists, and the loop's
/// own parser takes them: `--metal-host`'s where the line names one, and else
/// the bench's own.
pub fn the_bench_reaches_the_loop_as_its_host() {
    let suite: Vec<String> =
        ["--metal", "--metal-host", "dev@box"].iter().map(ToString::to_string).collect();
    let named = toyos_metal::testargs::parse(&suite).expect("a suite line").metal_host;
    let (image, home) = (Path::new("/i.img"), Path::new("/h"));
    for (bench, named, host) in
        [(Bench::T14, named, "dev@box"), (Bench::T14, None, "toyos@bench-t14"), (Bench::Amd, None, "toyos@bench-amd")]
    {
        let (_, target) = metal::target_of(&MetalMode::Drive, Some(bench), named).expect("a machine for the bench");
        let words = metal::invocation(image, home, &target);
        let after = words.iter().position(|w| w == "--").expect("cargo's own words end") + 1;
        let tail = &words[after..];
        let at = tail.iter().position(|w| w == "--host").expect("--host forwarded");
        assert_eq!(tail[at + 1], host, "{words:?}");
        toyos_metal::Args::parse(tail).expect("toyos-metal takes the words it is handed");
    }
}

/// A `loader.log` that handed its boot a kernel.
fn loader() -> String {
    format!("{}\n{}\n{}\n", bootlog::LOADER_FIRST_LINE, bootlog::LOADER_CLOCK_NONE, bootlog::LOADER_LAST_LINE)
}

/// What every boot that reaches `Boot: complete` owes `one_clock`.
const STARTED: &str = "[2026-09-29 18:22:38  0.050 cpu0 kernel] spawn: pid=6 (total=0ms) /system/bin/logkeeper\n\
                       [2026-09-29 18:22:38  0.050 supervisor] supervisor: started logkeeper pid=6\n";

/// When [`boot_file`]'s boot came back, by its host's clock: 2026-09-29
/// 18:23:19 UTC, 40 s after [`BOOTED`]'s date.
const BACK_AT: u64 = 1_790_706_199;

/// A T14 boot's `boot.txt` as the loop writes it.
pub(super) fn boot_file() -> String {
    let phases = Phases {
        preamble: 3.1,
        flash: 121.5,
        dd_secs: 117.2,
        entry: 2.3,
        down: 8.0,
        read_log: 1.4,
        raw_log: Some(9.9),
    };
    BootFile { back: 40, back_at: BACK_AT, machine: t14(), phases }.text()
}

/// One T14 boot's readback as the loop writes it.
pub(super) fn plant(dir: &Path, label: &str, kernel: &str, verdict: Option<&Refusal>) {
    plant_on(ON_T14, dir, label, kernel, verdict);
}

/// The same of the bench whose first record `on` is.
fn plant_on(on: &str, dir: &Path, label: &str, kernel: &str, verdict: Option<&Refusal>) {
    let home = metal::at(dir, label);
    fs::create_dir_all(&home).expect("a readback directory");
    for (name, text) in [
        (READBACK_LOADER, loader()),
        (READBACK_KERNEL, format!("{on}{STARTED}{kernel}")),
        (READBACK_BOOT, boot_file()),
        (READBACK_VERDICT, verdict_file(verdict)),
    ] {
        fs::write(home.join(name), text).expect("a planted file");
    }
}

/// One boot the loop passed, read back.
fn planted(kernel: &str) -> Readback {
    let dir = toyos_tmpdir::TempDir::new("metal-readback");
    plant(&dir, "planted", kernel, None);
    metal::read_readback(&dir, "planted").expect("a planted readback")
}

fn read(dir: &Path, labels: &[&str]) -> BTreeMap<String, Result<Readback, String>> {
    labels.iter().map(|label| (label.to_string(), metal::read_readback(dir, label))).collect()
}

pub fn a_name_measured_twice_is_refused() {
    let back = planted(BOOTED);
    assert_eq!(back.measured("span.planted.us", 1), Ok(()));
    let why = back.measured("span.planted.us", 2).expect_err("a second value");
    assert!(why.contains("twice"), "{why}");
}

fn span(b: &[&Readback]) -> Result<(), String> {
    b[0].measured(&format!("span.{}.us", b[0].label), 7)
}

fn span_and_fail(b: &[&Readback]) -> Result<(), String> {
    span(b)?;
    Err("a planted failure".to_string())
}

fn unqualified(b: &[&Readback]) -> Result<(), String> {
    b[0].measured("latency.p99_us", 16)
}

fn unqualified_and_fail(b: &[&Readback]) -> Result<(), String> {
    unqualified(b)?;
    Err("a planted failure".to_string())
}

static PASSING: Metal =
    Metal { on: On::Every, arms: &[metal::once("passing", "tests/images/jobcase.toml", &[], &[])], judge: span };
static FAILING: Metal = Metal {
    on: On::Every,
    arms: &[metal::once("failing", "tests/images/jobcase.toml", &[], &[])],
    judge: span_and_fail,
};
static BESIDE: Metal = Metal {
    on: On::Every,
    arms: &[metal::once("failing", "tests/images/jobcase.toml", &[], &[])],
    judge: |b| b[0].measured("beside.failing.us", 9),
};
static REFUSED: Metal =
    Metal { on: On::Every, arms: &[metal::once("refused", "tests/images/jobcase.toml", &[], &[])], judge: span };
static SKEWED: Metal =
    Metal { on: On::Every, arms: &[metal::once("skewed", "tests/images/jobcase.toml", &[], &[])], judge: span };
static ONE: Metal =
    Metal { on: On::Every, arms: &[metal::once("one", "tests/images/jobcase.toml", &[], &[])], judge: unqualified };
static ONE_FAILS: Metal = Metal {
    on: On::Every,
    arms: &[metal::once("one", "tests/images/jobcase.toml", &[], &[])],
    judge: unqualified_and_fail,
};
static TWO: Metal =
    Metal { on: On::Every, arms: &[metal::once("two", "tests/images/jobcase.toml", &[], &[])], judge: unqualified };

/// **A number is recorded off its owner's verdict and nobody else's.** A row
/// that fails keeps its own number out of the record, and neither its boot's
/// numbers nor those of the row beside it on that boot; a boot whose own check
/// fails keeps its own out, and not those of the row that passed on it, unless
/// the check is of its clocks, which every duration on it was read by; a boot
/// the loop refused measured nothing.
pub fn a_number_fails_with_its_owner_alone() {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = recorded();
    plant(&dir, "passing", BOOTED, None);
    plant(&dir, "failing", BOOTED, None);
    plant(&dir, "refused", BOOTED, Some(&Refusal::Log(toyos_build::bootlog::Unfit::NoStopMark)));
    // The supervisor's line 10 ms before the spawn it reports.
    plant(&dir, "skewed", BOOTED, None);
    let at = metal::at(&dir, "skewed").join(READBACK_KERNEL);
    let kernel = fs::read_to_string(&at).expect("a planted log");
    fs::write(&at, kernel.replacen("0.050 supervisor]", "0.040 supervisor]", 1)).expect("a planted log");
    let readbacks = read(&dir, &["passing", "failing", "refused", "skewed"]);
    assert!(readbacks["skewed"].as_ref().expect("a readback").one_clock().is_err());
    let why = readbacks["refused"].as_ref().err().expect("the loop's refusal, read back");
    assert!(why.contains("never made the log durable at a stop"), "{why}");

    let tests = [
        ("passes", &PASSING),
        ("fails", &FAILING),
        ("beside", &BESIDE),
        ("refused", &REFUSED),
        ("skewed", &SKEWED),
    ];
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    assert!(metal::judge_readbacks(&root, Bench::T14, &readbacks, &runs, &[]), "four failures judged green");
    let record = Record::load(&root, &t14()).expect("a readable record").expect("a record");
    let names: Vec<&str> = record.measured.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "beside.failing.us",
            "boot.failing.back_secs",
            "boot.failing.complete_ms",
            "boot.passing.back_secs",
            "boot.passing.complete_ms",
            "span.passing.us"
        ]
    );
    // The row's own failure is red with nothing else beside it.
    let alone = [("fails", &FAILING)];
    let runs: Vec<&(&str, &'static Metal)> = alone.iter().collect();
    let root = recorded();
    assert!(
        metal::judge_readbacks(&root, Bench::T14, &read(&dir, &["failing"]), &runs, &[]),
        "a failing row judged green"
    );
}

static HOLED: Metal =
    Metal { on: On::Every, arms: &[metal::once("holed", "tests/images/jobcase.toml", &[], &[])], judge: span };

/// **A boot whose log is missing parts of itself is refused by name, and no
/// row is judged on what is left**: `span` asserts no line, so it would pass
/// over the hole. The same boot having rotated and lost nothing is read.
pub fn a_boot_that_lost_parts_of_its_log_judges_no_row() {
    use toyos_logstream::{LOG_CONTINUES, LOG_OPENED};
    const STEM: &str = "2026-09-29-182239";
    let part = |part| toyos_wallclock::Part { stem: STEM, part };
    let opened = format!("[2026-09-29 18:22:39  1.170 logkeeper] {LOG_OPENED}/log/{} (2026-09-29 18:22:39 UTC)\n", part(1));
    let continued = |to: u32| {
        format!(
            "[2026-09-29 18:23:14 36.901 logkeeper] logkeeper: /log/{} reached 1048816{LOG_CONTINUES}/log/{}\n",
            part(to - 1),
            part(to)
        )
    };
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = recorded();
    // Parts 1, 4 and 5 came back, and no line says where 2 and 3 went.
    plant(&dir, "holed", &format!("{BOOTED}{opened}{}{}", continued(4), continued(5)), None);
    plant(&dir, "passing", &format!("{BOOTED}{opened}{}{}", continued(2), continued(3)), None);
    let readbacks = read(&dir, &["holed", "passing"]);
    assert_eq!(
        readbacks["holed"].as_ref().err().map(String::as_str),
        Some("holed's own log is missing its parts 2 to 3; no row is judged on it")
    );

    let tests = [("holed", &HOLED), ("passes", &PASSING)];
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    assert!(metal::judge_readbacks(&root, Bench::T14, &readbacks, &runs, &[]), "a boot with a hole in its log judged green");
    let record = Record::load(&root, &t14()).expect("a readable record").expect("a record");
    assert!(record.measured.contains_key("span.passing.us"), "{:?}", record.measured);
    assert!(!record.measured.keys().any(|name| name.contains("holed")), "{:?}", record.measured);
}

/// Two rows under one name are judged on the first reading and recorded off
/// neither, whether the first of them passed or failed.
pub fn a_name_two_owners_measured_is_refused() {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    plant(&dir, "one", BOOTED, None);
    plant(&dir, "two", BOOTED, None);
    let readbacks = read(&dir, &["one", "two"]);
    for first in [&ONE, &ONE_FAILS] {
        let root = recorded();
        let tests = [("one", first), ("two", &TWO)];
        let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
        assert!(metal::judge_readbacks(&root, Bench::T14, &readbacks, &runs, &[]), "one name twice judged green");
        let record = Record::load(&root, &t14()).expect("a readable record").expect("a record");
        assert!(!record.measured.contains_key("latency.p99_us"), "{:?}", record.measured);
        assert_eq!(record.measured.len(), 4, "{:?}", record.measured);
    }
}

/// `passing`'s three rows under `bios`: its planted readings, with
/// `complete_ms` and `back_secs` as given.
fn kept_back(bios: &str, complete_ms: u64, back_secs: u64) -> Record {
    let machine = t14();
    Record {
        vendor: machine.vendor,
        product: machine.product,
        bios: bios.to_string(),
        measured: [
            ("boot.passing.back_secs", back_secs),
            ("boot.passing.complete_ms", complete_ms),
            ("span.passing.us", 7),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect(),
    }
}

/// [`kept_back`] at the 40 s the planted boot file came back in.
fn kept(bios: &str, complete_ms: u64) -> Record {
    kept_back(bios, complete_ms, 40)
}

/// `passing`'s readback judged against `record`, saved first: whether the
/// run was red, and the record it left.
fn judged_against(record: &Record) -> (bool, Record) {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = toyos_tmpdir::TempDir::new("metal-records");
    plant(&dir, "passing", BOOTED, None);
    record.save(&root).expect("a saved record");
    let tests = [("passes", &PASSING)];
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    let red = metal::judge_readbacks(&root, Bench::T14, &read(&dir, &["passing"]), &runs, &[]);
    (red, Record::load(&root, &t14()).expect("a readable record").expect("a record"))
}

/// **A reading past its record reds the run and moves nothing**: one boot,
/// green against a record it meets and red against one it is past.
pub fn a_reading_past_its_record_fails_and_moves_nothing() {
    let bios = t14().bios;
    let met = kept(&bios, 1165);
    assert_eq!(judged_against(&met), (false, met));
    // 1165 ms against a ceiling of 1164.
    let past = kept(&bios, 582);
    assert_eq!(judged_against(&past), (true, past));
    // **The come-back half of the contract**: back in 40 s against a ceiling
    // of 38, which is a stop the deadline ended rather than a clean one.
    let late = kept_back(&bios, 1165, 19);
    assert_eq!(judged_against(&late), (true, late));
}

/// **A run under another BIOS reds and records nothing**, though every reading
/// meets the record; the same record under this run's own BIOS is green and
/// gains the row it lacks.
pub fn a_run_under_another_bios_fails_and_records_nothing() {
    let lacking = |bios: &str| {
        let mut record = kept(bios, 1165);
        record.measured.remove("span.passing.us");
        record
    };
    let older = lacking("N34ET50W (1.50 )");
    assert_eq!(judged_against(&older), (true, older));
    let bios = t14().bios;
    assert_eq!(judged_against(&lacking(&bios)), (false, kept(&bios, 1165)));
}

fn member(job: &str, adds_ms: u64) -> metal::Member {
    metal::Member { job: job.to_string(), adds_ms }
}

/// **A failing shared member fails itself alone**: the run is red, and the
/// boot it rode records its own numbers as the boot whose members all passed
/// does.
pub fn a_failing_shared_member_fails_itself_alone() {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = recorded();
    let jobs = ["test_rs_std_tls_multi_crate", "test_rs_a_failing_member"];
    let exits = |code: i32| {
        format!(
            "{BOOTED}[2026-09-29 18:22:41  2.310 cpu3 kernel] exit: {} pid=12 code=0 cpu=4ms\n\
             [2026-09-29 18:22:42  3.120 cpu5 kernel] exit: {} pid=13 code={code} cpu=9ms\n",
            jobs[0], jobs[1]
        )
    };
    plant(&dir, "passing", &exits(0), None);
    plant(&dir, "failing", &exits(101), None);
    let shared = |boot: &str| metal::SharedBoot {
        boot: boot.to_string(),
        config: "tests/images/testcases.toml",
        params: &[],
        features: &[],
        members: jobs.iter().map(|job| member(job, toyos_tco::RUST_MEMBER_MS)).collect(),
        files: Vec::new(),
        links: Vec::new(),
    };
    let readbacks = read(&dir, &["passing", "failing"]);
    assert!(
        metal::judge_readbacks(&root, Bench::T14, &readbacks, &[], &[shared("passing"), shared("failing")]),
        "a failing member judged green"
    );
    let record = Record::load(&root, &t14()).expect("a readable record").expect("a record");
    let names: Vec<&str> = record.measured.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["boot.failing.back_secs", "boot.failing.complete_ms", "boot.passing.back_secs", "boot.passing.complete_ms"]
    );
}

/// **A boot's last job is behind every other, whichever row names it first**:
/// a row after the one that ends the boot adds its job before the last, and
/// two rows ending one boot on different jobs are refused by both names.
pub fn a_boots_last_job_is_behind_every_other() {
    const HELD: metal::Arm = metal::Arm { last: Some("hold"), ..metal::once("own", "tests/images/testcases.toml", &[], &["early"]) };
    static ENDS: Metal = Metal { on: On::Every, arms: &[HELD], judge: |_| Ok(()) };
    static LATER: Metal =
        Metal { on: On::Every, arms: &[metal::once("own", "tests/images/testcases.toml", &[], &["hold", "later"])], judge: |_| Ok(()) };
    static OTHER: Metal = Metal {
        on: On::Every,
        arms: &[metal::Arm { last: Some("later"), ..metal::once("own", "tests/images/testcases.toml", &[], &[]) }],
        judge: |_| Ok(()),
    };
    let boots = metal::batches(&[("ends", &ENDS), ("later", &LATER)], &[]).expect("one last job");
    assert_eq!(boots["own"].jobs, ["early", "later", "hold"]);
    let Err(refused) = metal::batches(&[("ends", &ENDS), ("other", &OTHER)], &[]) else {
        panic!("two last jobs on one boot were batched");
    };
    assert!(refused.contains("other ends the boot \"own\" on later and another row ends it on hold"), "{refused}");
}

/// **A boot's rows' jobs run before its members, the rows' last job among
/// them, and its bounds follow from who rides it**: the runner's bound over
/// the list is the rows' and what each member adds, the kernel's over the boot
/// twice that, and a boot no member rides keeps the bounds a row's boot had. A
/// shared boot that disagrees with a row about the image is refused, and so
/// are two under one name, and two jobs of one boot the kernel records under
/// one name.
pub fn rows_run_before_members_under_a_bound_the_members_widen() {
    static EARLY: Metal =
        Metal { on: On::Every, arms: &[metal::once("shared", "tests/images/testcases.toml", &[], &["tone", "cost"])], judge: |_| Ok(()) };
    static HELD: Metal = Metal {
        on: On::Every,
        arms: &[
            metal::Arm { last: Some("hold"), ..metal::once("shared", "tests/images/testcases.toml", &[], &["late"]) },
            metal::once("own", "tests/images/testcases.toml", &[], &["alone"]),
        ],
        judge: |_| Ok(()),
    };
    let members = |boot: &str, members: &[(&str, u64)]| metal::SharedBoot {
        boot: boot.to_string(),
        config: "tests/images/testcases.toml",
        params: &[],
        features: &[],
        members: members.iter().map(|(job, adds_ms)| member(job, *adds_ms)).collect(),
        files: vec![(format!("expect/{}", members[0].0), Vec::new())],
        links: Vec::new(),
    };
    let rows = [("early", &EARLY), ("held", &HELD)];
    let shared = [
        members("shared", &[("m1", 860), ("c1", 260), ("m2", 860)]),
        members("corpus", &[("c2", 260), ("c3", 260)]),
    ];
    let boots = metal::batches(&rows, &shared).expect("one image a boot");
    let bounds = |boot: &str| (boots[boot].bound_ms(), boots[boot].deadline_ms());
    assert_eq!(boots["shared"].jobs, ["tone", "cost", "late", "hold", "m1", "c1", "m2"]);
    assert_eq!(bounds("shared"), (61_980, 123_960));
    assert_eq!(boots["corpus"].jobs, ["c2", "c3"]);
    assert_eq!(bounds("corpus"), (60_520, 121_040));
    // No member: the bounds every row's boot had before a list could widen.
    assert_eq!(bounds("own"), (60_000, 120_000));

    let debug = metal::SharedBoot { features: toyos_build::build::TEST_KERNEL, ..shared[0].clone() };
    let Err(refused) = metal::batches(&rows, &[debug]) else {
        panic!("a shared boot rode a row's boot on another kernel");
    };
    assert!(refused.contains("the shared boot \"shared\" rides the boot \"shared\" as"), "{refused}");
    let Err(refused) = metal::batches(&rows, &[shared[0].clone(), shared[0].clone()]) else {
        panic!("two shared boots under one name were batched");
    };
    assert!(refused.contains("two shared boots are both named \"shared\""), "{refused}");
    let long = members("shared", &[("test_rs_a_name_past_what_is_kept", 860), ("test_rs_a_name_past_what_is_lost", 860)]);
    let Err(refused) = metal::batches(&rows, &[long]) else {
        panic!("two jobs the kernel records under one name were batched");
    };
    assert!(refused.contains("the kernel records both as \"test_rs_a_name_past_what_is\""), "{refused}");
    // The kernel records a process by its binary, not its command line.
    let argued = members("shared", &[("disks designate data --commit", 860), ("disks list", 860)]);
    let Err(refused) = metal::batches(&rows, &[argued]) else {
        panic!("two jobs of one binary were batched");
    };
    assert!(refused.contains("the kernel records both as \"disks\""), "{refused}");
}

/// **What a run's words take**: no word the whole profile but its named-only
/// rows, which only their whole name or their boot takes; a name word every
/// row and member it is part of, and of a shared boot only the files its
/// members name; a boot word that boot whole, with every row that rides it,
/// whatever name words stand beside it; and a word that takes nothing is
/// refused.
pub fn words_take_rows_members_and_whole_boots() {
    static ROWS: [(&str, Metal); 2] = [
        ("alpha_row", Metal { on: On::Every, arms: &[metal::once("own", "tests/images/testcases.toml", &[], &[])], judge: |_| Ok(()) }),
        (
            "beta_row",
            Metal {
                on: On::Every,
                arms: &[metal::Arm { named_only: true, ..metal::once("shared", "tests/images/testcases.toml", &[], &[]) }],
                judge: |_| Ok(()),
            },
        ),
    ];
    let boot = |boot: &str, jobs: &[&str]| metal::SharedBoot {
        boot: boot.to_string(),
        config: "tests/images/testcases.toml",
        params: &[],
        features: &[],
        members: jobs.iter().map(|job| member(job, toyos_tco::RUST_MEMBER_MS)).collect(),
        files: jobs
            .iter()
            .flat_map(|job| [(format!("expect/{job}"), Vec::new()), (format!("bin/test_c_{job}"), Vec::new())])
            .collect(),
        links: jobs.iter().map(|job| (format!("bin/{job}"), "/system/bin/test_rs_ccheck".to_string())).collect(),
    };
    let profile = [boot("shared", &["test_rs_a1", "test_rs_a2", "test_rs_b1"]), boot("ccorpus", &["c1", "c2"])];
    let jobs_of = |boot: &metal::SharedBoot| boot.members.iter().map(|m| m.job.clone()).collect::<Vec<_>>();
    let taken = |names: &[&str], boots: &[&str]| {
        metal::select(names, boots, Bench::T14, &ROWS, &profile).map(|taken| {
            assert!(taken.left.is_empty(), "{:?}", taken.left);
            let rows: Vec<&str> = taken.rows.iter().map(|(name, _)| *name).collect();
            let shared: Vec<String> =
                taken.shared.iter().map(|boot| format!("{}={}", boot.boot, jobs_of(boot).join("+"))).collect();
            (rows.join(","), shared.join(","))
        })
    };
    let took = |rows: &str, shared: &str| Ok::<_, String>((rows.to_string(), shared.to_string()));
    // `beta_row` is named-only: no word, a part of its name or a word for
    // another row leaves it, its whole name or its boot takes it.
    assert_eq!(taken(&[], &[]), took("alpha_row", "shared=test_rs_a1+test_rs_a2+test_rs_b1,ccorpus=c1+c2"));
    assert_eq!(taken(&["_row"], &[]), took("alpha_row", ""));
    assert_eq!(taken(&["beta_row"], &[]), took("beta_row", ""));
    assert_eq!(taken(&["b1"], &[]), took("", "shared=test_rs_b1"));
    assert_eq!(taken(&["alpha", "c1"], &[]), took("alpha_row", "ccorpus=c1"));
    assert_eq!(taken(&[], &["shared"]), took("beta_row", "shared=test_rs_a1+test_rs_a2+test_rs_b1"));
    // One boot, once and whole, where a name word takes a member of it too.
    assert_eq!(taken(&["a2"], &["shared"]), took("beta_row", "shared=test_rs_a1+test_rs_a2+test_rs_b1"));
    assert_eq!(taken(&["alpha"], &["ccorpus"]), took("alpha_row", "ccorpus=c1+c2"));
    assert_eq!(taken(&[], &["own"]), took("alpha_row", ""));
    // A member taken by name brings its own files and links and no other's.
    let shared = metal::select(&["c2"], &[], Bench::T14, &ROWS, &profile).expect("a member's name").shared;
    let paths: Vec<&str> = shared[0].files.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(paths, ["expect/c2", "bin/test_c_c2"]);
    assert_eq!(shared[0].links, [("bin/c2".to_string(), "/system/bin/test_rs_ccheck".to_string())]);
    let refused: [(&[&str], &[&str], &str); 4] = [
        (&["a1", "nope"], &[], "\"nope\" is part of no"),
        (&["rs_a1"], &[], "\"rs_a1\" is part of no"),
        (&["beta"], &[], "\"beta\" is part of no"),
        (&[], &["shared", "shared-2"], "boot:shared-2 names no boot"),
    ];
    for (names, boots, refusal) in refused {
        let said = taken(names, boots).expect_err("a word that takes nothing was accepted");
        assert!(said.contains(refusal), "{said}");
    }
}

/// **The loader's lines, the kernel's records and a program's lines count
/// from one zero**: a kernel record stamped before the loader's handoff, or the
/// supervisor's line after a spawn before that spawn's record, is a clock that kept another
/// zero; and a boot that owes something to compare and lacks it is refused.
pub fn the_loader_the_kernel_and_a_program_count_from_one_zero() {
    const STATED: &str = "Loader clock: each line opens with the seconds since the counter's zero, at \
                          the counter's stated 2419200000 Hz";
    let read = |clock: &str, loader_ms: &str, kernel_ms: &str, said: &str| {
        let dir = toyos_tmpdir::TempDir::new("metal-one-clock");
        let kernel = format!(
            "{ON_T14}[2026-10-04 09:30:00 {kernel_ms} cpu0 kernel] panic console: armed\n\
             [2026-10-04 09:30:00 11.900 cpu0 kernel] spawn: pid=5 (total=0ms) /system/bin/logkeeper\n\
             [2026-10-04 09:30:00 12.000 cpu0 kernel] spawn: pid=6 (total=0ms) /system/bin/logkeeper\n\
             {said}\
             [2026-10-04 09:30:00 15.000 cpu0 kernel] Boot: complete (3335ms)\n"
        );
        plant(&dir, "planted", &kernel, None);
        let loader = format!(
            "[{loader_ms} cpu0 loader] {}\n[{loader_ms} cpu0 loader] {clock}\n[{loader_ms} cpu0 loader] {}\n",
            bootlog::LOADER_FIRST_LINE,
            bootlog::LOADER_LAST_LINE,
        );
        let home = metal::at(&dir, "planted");
        fs::write(home.join(READBACK_LOADER), loader).expect("a planted loader.log");
        fs::write(home.join(READBACK_KERNEL), kernel).expect("a planted log");
        metal::read_readback(&dir, "planted").expect("a planted readback").one_clock()
    };
    let (stated, none) = (STATED, bootlog::LOADER_CLOCK_NONE);
    let said = |ms: &str| format!("[2026-10-04 09:30:00 {ms} supervisor] {}6\n", bootlog::LOGKEEPER_STARTED);
    assert_eq!(read(stated, " 9.876", "11.665", &said("13.064")), Ok(()));
    assert_eq!(read(none, "--.---", " 0.050", &said("13.064")), Ok(()));
    let why = read(stated, " 9.876", " 0.050", &said("13.064")).expect_err("a kernel that counts from its own start");
    assert!(why.contains("the kernel's earliest timed record 50 ms"), "{why}");
    let why = read(stated, " 9.876", "11.665", &said(" 1.064")).expect_err("a program that counts from the kernel's clock");
    assert!(why.contains("reads 1064 ms and the kernel's record of that spawn 12000 ms"), "{why}");
    // Nothing to compare, where the boot owes it.
    let why = read(stated, "--.---", "11.665", &said("13.064")).expect_err("a loader that lost its time");
    assert!(why.contains("none of its lines carries a time"), "{why}");
    let why = read("Loader log: opened", " 9.876", "11.665", &said("13.064")).expect_err("a loader silent on its clock");
    assert!(why.contains("said nothing of its clock"), "{why}");
    let why = read(stated, " 9.876", "11.665", "").expect_err("a complete boot with no supervisor line");
    assert!(why.contains("no timed supervisor line saying so"), "{why}");
}

/// **A job's own end with no code is no code**: its lowest-pid record is the
/// job's, and a kill or a fault there answers an error naming it, never a
/// child's exit code read in its place.
pub fn an_exit_record_without_a_code_answers_no_code() {
    let job = "test_rs_a_killed_job";
    let kernel = |end: &str| {
        format!(
            "{BOOTED}[2026-09-29 18:22:41  2.310 cpu3 kernel] exit: {job} pid=12 {end} cpu=4ms\n\
             [2026-09-29 18:22:42  3.120 cpu5 kernel] exit: {job} pid=13 code=0 cpu=9ms\n"
        )
    };
    assert_eq!(planted(&kernel("code=3")).exit_code(job), Ok(3));
    for end in ["killed", "fault=memory"] {
        let answer = planted(&kernel(end)).exit_code(job);
        assert!(answer.as_ref().is_err_and(|why| why.contains(end)), "{end}: {answer:?}");
    }
}

/// **On a bench booted off its own disk, the disk judge admits the volumes the
/// loader named and DATA, and nothing else of that disk**: a session on the
/// host's ESP, of the same type as ToyOS's, is refused, and so is every
/// session but DATA's where the loader's lines are not read.
pub fn the_bench_disk_judge_admits_data_and_the_loaders_volumes_alone() {
    let guid = |text: &str| toyos_gpt::Guid::parse(text).expect("a GUID");
    let (host_esp, data, esp, log) = (
        guid("BACB3F81-3674-441E-80A9-643C279215E3"),
        guid("771E4813-696F-4DEA-828A-522A0A822331"),
        guid("855A1AD8-B730-4CD0-B949-19D93B0DEF53"),
        guid("CC672C8C-965F-4C00-8E4C-C039D06C3CDB"),
    );
    let bytes = |g: toyos_gpt::Guid| g.0.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(", ");
    let loader = serial::Serial::named(
        "loader.log",
        format!(
            "[10.405 cpu0 loader] Boot partition: LBA 2048+69632 signature [{}]\n\
             [10.416 cpu0 loader] Log partition: signature [{}]\n",
            bytes(esp),
            bytes(log)
        ),
    );
    let ours = common::hostdisk::loader_named(&loader).expect("the loader's two lines");
    assert_eq!(ours, [esp, log]);
    let part = |g: toyos_gpt::Guid, kind: toyos_gpt::Guid| format!("[16.267 nvme] nvme 04:00.0: partition {g} of type {kind} at block 256, 8704 blocks\n");
    let session = |n: u32, g: toyos_gpt::Guid| format!("[16.578 nvme] nvme 04:00.0: session {n} opened {g} (8704 blocks)\n");
    let table = [
        BOOTED.to_string(),
        "[16.219 nvme] nvme 04:00.0: up: 4 I/O queues of 63 commands\n".to_string(),
        part(host_esp, toyos_gpt::Guid::EFI_SYSTEM),
        part(data, toyos_gpt::Guid::TOYOS_DATA),
        part(esp, toyos_gpt::Guid::EFI_SYSTEM),
        part(log, toyos_gpt::Guid::MICROSOFT_BASIC),
    ]
    .concat();
    let tail = "[16.600 diskserver] diskserver: every disk driver has settled\n\
                [16.652 fileserver] fileserver: mounted the DATA volume\n\
                [16.677 fileserver] fileserver: Data serving /apps — bcachefs, 14 names\n";
    let booted = |sessions: &[String]| serial::Serial::named("the boot", format!("{table}{}{tail}", sessions.concat()));
    let ours_and_data = booted(&[session(0, data), session(1, log), session(2, esp)]);
    assert_eq!(common::hostdisk::left_alone(&ours_and_data, &ours), Ok((4, common::hostdisk::Data::Mounted)));
    assert!(common::hostdisk::left_alone(&ours_and_data, &[]).is_err(), "the loader's volumes admitted unnamed");
    let host = booted(&[session(0, data), session(1, host_esp)]);
    let why = common::hostdisk::left_alone(&host, &ours).unwrap_err();
    assert!(why.contains(&host_esp.to_string()), "{why}");
    assert!(common::hostdisk::loader_named(&serial::Serial::named("loader.log", "Loading kernel elf...\n")).is_err());
}

fn passes(_: &[&Readback]) -> Result<(), String> {
    Ok(())
}

/// Four rows of one profile: one every bench runs, one for each bench's own
/// hardware on the boot the first rides, and one written for the AMD bench on
/// a boot of its own.
static BENCH_ROWS: [(&str, Metal); 4] = [
    (
        "every_row",
        Metal { on: On::Every, arms: &[metal::once("own", "tests/images/testcases.toml", &[], &["common"])], judge: passes },
    ),
    (
        "wired_row",
        Metal {
            on: On::Having(&[Device::I219]),
            arms: &[metal::once("own", "tests/images/testcases.toml", &[], &["wired"])],
            judge: passes,
        },
    ),
    (
        "amdvi_row",
        Metal {
            on: On::Having(&[Device::AmdVi]),
            arms: &[metal::once("own", "tests/images/testcases.toml", &[], &["remapped"])],
            judge: passes,
        },
    ),
    (
        "amd_row",
        Metal {
            on: On::Only(Bench::Amd),
            arms: &[metal::once("amd-own", "tests/images/testcases.toml", &[], &["idle"])],
            judge: passes,
        },
    ),
];

/// **A bench's boot carries its own rows' jobs and no other's**: a row the
/// bench does not run is left by name with why, whether no word, a name or a
/// boot would have taken it, its jobs are in no list, and a run that takes
/// only such rows is refused.
pub fn a_bench_takes_its_own_rows_and_names_the_rest() {
    let taken = |names: &[&str], boots: &[&str], bench| {
        let taken = metal::select(names, boots, bench, &BENCH_ROWS, &[])?;
        let rows: Vec<&str> = taken.rows.iter().map(|(name, _)| *name).collect();
        let left: Vec<String> = taken.left.iter().map(|(name, why)| format!("{name} {why}")).collect();
        let boots = metal::batches(&taken.rows, &taken.shared)?;
        let jobs: Vec<String> = boots.iter().map(|(boot, batch)| format!("{boot}={}", batch.jobs.join("+"))).collect();
        Ok::<_, String>((rows.join(","), jobs.join(","), left.join("; ")))
    };
    let took = |rows: &str, jobs: &str, left: &str| Ok((rows.to_string(), jobs.to_string(), left.to_string()));
    let no_amdvi = "amdvi_row needs an AMD-Vi unit, which the T14 has not";
    let not_amd = "amd_row is written for the AMD bench alone";
    let no_i219 = "wired_row needs an I219, which the AMD bench has not";
    assert_eq!(
        taken(&[], &[], Bench::T14),
        took("every_row,wired_row", "own=common+wired", &format!("{no_amdvi}; {not_amd}"))
    );
    assert_eq!(
        taken(&[], &[], Bench::Amd),
        took("every_row,amdvi_row,amd_row", "amd-own=idle,own=common+remapped", no_i219)
    );
    assert_eq!(taken(&[], &["own"], Bench::Amd), took("every_row,amdvi_row", "own=common+remapped", no_i219));
    assert_eq!(taken(&["_row"], &[], Bench::T14), taken(&[], &[], Bench::T14));
    assert_eq!(taken(&["amd"], &[], Bench::Amd), took("amdvi_row,amd_row", "amd-own=idle,own=remapped", ""));
    assert_eq!(taken(&["every", "amd_row"], &[], Bench::T14), took("every_row", "own=common", not_amd));
    let refused: [(&[&str], &[&str], Bench, String); 3] = [
        (&["wired"], &[], Bench::Amd, format!("the run takes nothing on the AMD bench: {no_i219}")),
        (&["amd"], &[], Bench::T14, format!("the run takes nothing on the T14: {no_amdvi}; {not_amd}")),
        (&[], &["amd-own"], Bench::T14, format!("the run takes nothing on the T14: {not_amd}")),
    ];
    for (names, boots, bench, refusal) in refused {
        assert_eq!(taken(names, boots, bench), Err(refusal));
    }
}

/// The profile's own rows, by bench: the jobs that drive one bench's hardware
/// ride no boot of the other, and every row is some bench's.
pub fn the_profile_gives_each_bench_its_own_jobs() {
    let jobs_of = |boots: &[&str], bench| {
        let taken = metal::select(&[], boots, bench, METAL, &[]).expect("a selection");
        let rows: BTreeSet<&str> = taken.rows.iter().map(|(name, _)| *name).collect();
        let left: BTreeSet<&str> = taken.left.iter().map(|(name, _)| *name).collect();
        let boots = metal::batches(&taken.rows, &taken.shared).expect("one image a boot");
        (rows, left, boots)
    };
    let (t14_rows, t14_left, t14) = jobs_of(&[], Bench::T14);
    let (amd_rows, amd_left, amd) = jobs_of(&[], Bench::Amd);
    let has = |boots: &BTreeMap<String, metal::Batch>, boot: &str, job: &str| boots[boot].jobs.iter().any(|j| j == job);
    // The I219's, the T14's alone.
    for job in [claims::RECLAIM, claims::MSI, "test_rs_counters_metal", "test_rs_acpi_counted"] {
        assert!(has(&t14, "testcases", job) && !has(&amd, "testcases", job), "{job}");
    }
    assert!(has(&t14, "benchcase", "test_rs_lan_lease") && !has(&amd, "benchcase", "test_rs_lan_lease"));
    // The AMD bench's alone: its DMA fault, and its idle states.
    for job in ["test_rs_usbd_spare_fault", "test_rs_usbd_spare_vector", "test_rs_idle_entered"] {
        assert!(has(&amd, "testcases", job) && !has(&t14, "testcases", job), "{job}");
    }
    assert!(t14_left.contains("amdvi_usbd_drives_its_own_entry") && amd_rows.contains("amdvi_usbd_drives_its_own_entry"));
    assert!(amd_left.contains("i219_carries_netstacks_link") && t14_rows.contains("i219_carries_netstacks_link"));
    assert!(t14_rows.is_disjoint(&t14_left) && amd_rows.is_disjoint(&amd_left));
    assert!(t14_left.is_subset(&amd_rows) && amd_left.is_subset(&t14_rows), "a row no bench runs");
    // A boot named whole takes its bench's rows of it.
    let (rows, left, _) = jobs_of(&["benchcase"], Bench::Amd);
    assert!(rows.contains("osbench") && !rows.iter().any(|row| row.starts_with("i219_")), "{rows:?}");
    assert_eq!(left, BTreeSet::from(["i219_carries_netstacks_link", "i219_leaves_ulp_unarmed"]));
}

/// **A run's bench is its readbacks' where it has any**, whatever the line
/// names beside them but another bench, and the line's or the T14 with none;
/// **its machine is the bench's own** unless the line names one, and never
/// another bench's. So a directory one boot of two has answered, which is
/// staged again, is staged for its readback's bench and that bench's machine,
/// and the line cannot send a bench's boots to the other's machine.
pub fn a_runs_bench_is_its_readbacks_and_else_the_lines() {
    let (t14, amd) = (|| (Bench::T14, "toyos@bench-t14".to_string()), || (Bench::Amd, "toyos@bench-amd".to_string()));
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let offline = MetalMode::Offline(dir.to_path_buf());
    let dev = Some("dev@box");
    assert_eq!(metal::target_of(&offline, None, None), Ok(t14()));
    assert_eq!(metal::target_of(&offline, Some(Bench::Amd), None), Ok(amd()));
    assert_eq!(metal::target_of(&offline, None, dev), Ok((Bench::T14, "dev@box".to_string())));
    assert_eq!(metal::target_of(&offline, Some(Bench::Amd), dev), Ok((Bench::Amd, "dev@box".to_string())));
    assert_eq!(metal::target_of(&MetalMode::List, Some(Bench::Amd), None), Ok(amd()));
    assert_eq!(metal::target_of(&MetalMode::Drive, None, None), Ok(t14()));
    let why = metal::target_of(&offline, None, Some("toyos@bench-amd")).expect_err("the T14's rows on the AMD bench");
    assert!(why.contains("names bench-amd, the AMD bench's machine, and the run takes the T14's rows"), "{why}");

    // The AMD bench's `testcases` came back and its watchdog boot did not.
    plant_on(ON_AMD, &dir, "testcases", BOOTED, None);
    // A log that names no bench decides nothing here.
    fs::create_dir_all(dir.join("unnamed")).expect("a directory");
    fs::write(dir.join("unnamed").join(READBACK_KERNEL), BOOTED).expect("a planted log");
    assert_eq!(metal::target_of(&offline, None, None), Ok(amd()));
    assert_eq!(metal::target_of(&offline, Some(Bench::Amd), None), Ok(amd()));
    let why = metal::target_of(&offline, None, Some("toyos@bench-t14")).expect_err("the AMD bench's rows on the T14");
    assert!(why.contains("names bench-t14, the T14's machine, and the run takes the AMD bench's rows"), "{why}");
    let why = metal::target_of(&offline, Some(Bench::T14), None).expect_err("another bench than the readback's");
    assert!(why.contains("--metal-bench names the T14 and testcases's readback is from the AMD bench"), "{why}");

    // The mirror: the T14's readback, and the line naming the AMD bench's machine.
    let t14_dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    plant(&t14_dir, "testcases", BOOTED, None);
    let t14_offline = MetalMode::Offline(t14_dir.to_path_buf());
    assert_eq!(metal::target_of(&t14_offline, None, None), Ok(t14()));
    let why = metal::target_of(&t14_offline, None, Some("toyos@bench-amd")).expect_err("the T14's rows on the AMD bench");
    assert!(why.contains("names bench-amd, the AMD bench's machine, and the run takes the T14's rows"), "{why}");

    plant(&dir, "two", BOOTED, None);
    let why = metal::target_of(&offline, None, None).expect_err("two benches in one run");
    assert!(why.contains("testcases's readback from the AMD bench and two's from the T14"), "{why}");
}

/// **A judge names the bench its readback came from**, and a boot that ran on
/// another bench than the run's rows are for is red with every row on it.
pub fn a_judge_names_the_bench_it_judged() {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let failed = format!("{BOOTED}[2026-09-29 18:22:41  2.310 cpu3 kernel] exit: test_rs_a_job pid=12 code=101 cpu=4ms\n");
    plant_on(ON_AMD, &dir, "passing", &failed, None);
    plant(&dir, "failing", &failed, None);
    let readbacks = read(&dir, &["passing", "failing"]);
    let said = |label: &str| readbacks[label].as_ref().expect("a readback").job_passed("test_rs_a_job").unwrap_err();
    assert!(said("passing").starts_with("test_rs_a_job exited 101 on the AMD bench;"), "{}", said("passing"));
    assert!(said("failing").starts_with("test_rs_a_job exited 101 on the T14;"), "{}", said("failing"));
    assert!(read(&dir, &["passing"])["passing"].as_ref().is_ok_and(|back| back.bench == Bench::Amd));

    let tests = [("passes", &PASSING)];
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    assert!(!metal::judge_readbacks(&recorded(), Bench::Amd, &read(&dir, &["passing"]), &runs, &[]));
    let root = recorded();
    assert!(
        metal::judge_readbacks(&root, Bench::T14, &read(&dir, &["passing"]), &runs, &[]),
        "the AMD bench's boot judged green for the T14's rows"
    );
    let record = Record::load(&root, &t14()).expect("a readable record").expect("a record");
    assert!(record.measured.is_empty(), "a boot of another bench recorded {:?}", record.measured);

    // A log that names no bench is no readback.
    fs::write(metal::at(&dir, "failing").join(READBACK_KERNEL), format!("{STARTED}{BOOTED}")).expect("a planted log");
    let why = metal::read_readback(&dir, "failing").err().expect("a log naming no bench");
    assert_eq!(why, "failing's log names no bench: the kernel wrote no `microcode: cpu` record");
}

/// **Each bench's own readback, as its loop wrote it** (its `testcases` boot
/// of 2026-11-11, cut to the lines read here, its machine masked, its spawn
/// records and the supervisor's line in the form the kernel and the
/// supervisor now write them): it names
/// its bench, the real rows are taken for that bench and judged on it, and
/// the job that claims the T14's I219, which that boot of the AMD bench still
/// carried and which exited 1 there, is no row of the AMD bench's.
pub fn each_benchs_own_readback_is_judged_as_that_bench() {
    let masked = Machine { vendor: "masked".into(), product: "masked".into(), bios: "masked".into() };
    let judged = |bench: Bench, names: &[&str]| {
        let dir = compile::repo_root().join("tests/checks/readbacks").join(bench.word());
        assert_eq!(metal::target_of(&MetalMode::Offline(dir.clone()), None, None), Ok((bench, bench.host().to_string())));
        let taken = metal::select(names, &[], bench, METAL, &[])?;
        let root = toyos_tmpdir::TempDir::new("metal-records");
        Record::new(&masked).save(&root).expect("a recorded machine");
        let runs: Vec<&(&str, &'static Metal)> = taken.rows.iter().collect();
        let red = |as_bench| metal::judge_readbacks(&root, as_bench, &read(&dir, &["testcases"]), &runs, &[]);
        let other = if bench == Bench::T14 { Bench::Amd } else { Bench::T14 };
        assert!(red(other), "{bench}'s readback judged green as {other}'s");
        let rows: Vec<&str> = taken.rows.iter().map(|(name, _)| *name).collect();
        Ok::<_, String>((red(bench), rows.join(","), taken.left))
    };
    let reclaim = "claim_reuses_its_remapping_entry";
    assert_eq!(judged(Bench::T14, &["kernel_wx", reclaim]), Ok((false, format!("{reclaim},kernel_wx"), Vec::new())));
    assert_eq!(
        judged(Bench::Amd, &["kernel_wx", reclaim]),
        Ok((false, "kernel_wx".to_string(), vec![(reclaim, "needs an I219, which the AMD bench has not".to_string())]))
    );
    assert_eq!(
        judged(Bench::Amd, &[reclaim]).unwrap_err(),
        format!("the run takes nothing on the AMD bench: {reclaim} needs an I219, which the AMD bench has not")
    );
    let amd = metal::read_readback(&compile::repo_root().join("tests/checks/readbacks/amd"), "testcases").expect("a readback");
    let why = amd.job_passed(claims::RECLAIM).unwrap_err();
    assert!(why.starts_with("test_rs_pci_reclaim exited 1 on the AMD bench;"), "{why}");
}
