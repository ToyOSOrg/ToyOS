//! The metal harness's rules over planted readbacks, each written as the loop
//! writes one and read as a run reads one.

use super::*;
use metal::{Metal, Readback};
use toyos_build::metal::{
    verdict_file, Refusal, BACK_SECS, BIOS_KEY, PRODUCT_KEY, READBACK_BOOT, READBACK_KERNEL,
    READBACK_LOADER, READBACK_VERDICT, STICK_SECS_KEY, VENDOR_KEY,
};
use toyos_build::metaltimings::{Machine, Record};

const PANEL: &str = "| panel: paints=10 px=8741248 us=21751 max_us=3851\n";
const BOOTED: &str = "[2026-09-29 18:22:39 1.165 cpu0] Boot: complete (1165ms)\n";

fn t14() -> Machine {
    Machine {
        vendor: "LENOVO".to_string(),
        product: "20W0003AMZ".to_string(),
        bios: "N34ET71W (1.71 )".to_string(),
    }
}

/// A `loader.log` whose pass after the reset reads `page`.
fn loader(page: &str) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{page}",
        bootlog::LOADER_FIRST_LINE,
        bootlog::LOADER_LAST_LINE,
        bootlog::SEPARATOR,
        bootlog::LOADER_FIRST_LINE
    )
}

/// The record of arming the deadline, at the 60 ms the T14 arms it.
fn armed() -> String {
    format!(
        "[2026-09-29 18:22:39 0.060 cpu0] {}120000 ms, after which this kernel seals a WEDGED \
         record and writes the reset register itself\n{BOOTED}",
        bootlog::DEADLINE_ARMED
    )
}

fn expired_at(reached: i64) -> String {
    format!(
        "{PANEL}| {}: a bound of 120000 ms, reached at {reached} ms, with this machine in \
         `complete`.\n",
        bootlog::DEADLINE_EXPIRED
    )
}

/// One boot's readback as the loop writes it.
fn plant(dir: &Path, label: &str, page: &str, kernel: &str, verdict: Option<&Refusal>) {
    let home = metal::at(dir, label);
    fs::create_dir_all(&home).expect("a readback directory");
    let boot = format!(
        "{BACK_SECS} 40\n{STICK_SECS_KEY} 0\n{VENDOR_KEY} LENOVO\n{PRODUCT_KEY} 20W0003AMZ\n\
         {BIOS_KEY} N34ET71W (1.71 )\n"
    );
    for (name, text) in [
        (READBACK_LOADER, loader(page)),
        (READBACK_KERNEL, kernel.to_string()),
        (READBACK_BOOT, boot),
        (READBACK_VERDICT, verdict_file(verdict)),
    ] {
        fs::write(home.join(name), text).expect("a planted file");
    }
}

/// One boot the loop passed, read back.
fn planted(page: &str, kernel: &str) -> Readback {
    let dir = toyos_tmpdir::TempDir::new("metal-readback");
    plant(&dir, "planted", page, kernel, None);
    metal::read_readback(&dir, "planted").expect("a planted readback")
}

fn read(dir: &Path, labels: &[&str]) -> BTreeMap<String, Result<Readback, String>> {
    labels.iter().map(|label| (label.to_string(), metal::read_readback(dir, label))).collect()
}

/// **The stop's two refusals, each on the page that has to draw it**: a boot
/// that handed the machine back owes the stop's record, and a record with a
/// block operation still open is a sync about a machine that was still writing.
pub fn the_stop_owes_its_record_and_leaves_no_operation_open() {
    let whole = toyos_quiesce::Record {
        sweep: toyos_quiesce::Sweep { stopped: 6, running: 0 },
        elapsed_ms: 11,
        budget_ms: 2010,
        sweeps: 3,
        cpus: 8,
        in_flight: 0,
        begun: 4812,
    };
    let handed_back = |stop: &str| format!("Black box: {}\n{PANEL}{stop}", bootlog::HANDED_BACK);
    assert_eq!(planted(&handed_back(&format!("| {whole}\n")), BOOTED).stop_completed(), Ok(()));
    let why = planted(&handed_back(""), BOOTED)
        .stop_completed()
        .expect_err("a boot that handed the machine back with no record of its stop");
    assert!(why.contains("carries no record of the stop"), "{why}");
    let open = toyos_quiesce::Record { in_flight: 1, ..whole };
    let why = planted(&handed_back(&format!("| {open}\n")), BOOTED)
        .stop_completed()
        .expect_err("a stop that left an operation open");
    assert!(why.contains("1 block operation(s) open"), "{why}");
    let ended = format!("Previous boot's panic: the last boot read WEDGED\n{PANEL}");
    assert_eq!(planted(&ended, BOOTED).stop_completed(), Ok(()));
}

/// Each bound's lateness against the period of what polls it, a millisecond
/// either way and no further.
pub fn a_bound_fires_within_one_period_of_itself() {
    let quantum = i64::try_from(toyos_sched::fair::QUANTUM_NS / 1_000_000).expect("ms");
    let deadline = |late: i64| planted(&expired_at(120_060 + late), &armed()).deadline_on_time();
    assert_eq!(deadline(4), Ok(()));
    assert_eq!(deadline(quantum + 1), Ok(()));
    assert!(deadline(quantum + 2).is_err());
    assert_eq!(deadline(-1), Ok(()));
    assert!(deadline(-2).is_err());
    let unarmed = planted(&expired_at(120_064), BOOTED).deadline_on_time();
    assert!(unarmed.is_err(), "an expiry with no arm to count from passed");

    let sample = i64::try_from(toyos_tco::HARD_LOCKUP_SAMPLE_NS / 1_000_000).expect("ms");
    let lockup = |late: i64| {
        let page = format!(
            "{PANEL}| {}: cpu7 has taken no interrupt for {} ms, with `IF` clear at every \
             sample in that span. Its bound is 60000 ms.\n",
            bootlog::LOCKED_UP,
            60_000 + late
        );
        planted(&page, BOOTED).lockup_on_time()
    };
    assert_eq!(lockup(4), Ok(()));
    assert_eq!(lockup(sample + 1), Ok(()));
    assert!(lockup(sample + 2).is_err());

    let neither = planted(PANEL, BOOTED);
    assert_eq!((neither.deadline_on_time(), neither.lockup_on_time()), (Ok(()), Ok(())));
}

pub fn a_name_measured_twice_is_refused() {
    let back = planted(PANEL, BOOTED);
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

static PASSING: Metal =
    Metal { arms: &[metal::once("passing", "tests/jobcase", &[], &[])], judge: span };
static FAILING: Metal = Metal {
    arms: &[metal::once("failing", "tests/jobcase", &[], &[])],
    judge: span_and_fail,
};
static REFUSED: Metal =
    Metal { arms: &[metal::once("refused", "tests/jobcase", &[], &[])], judge: span };
static LATE: Metal =
    Metal { arms: &[metal::once("late", "tests/jobcase", &[], &[])], judge: span };
static ONE: Metal =
    Metal { arms: &[metal::once("one", "tests/jobcase", &[], &[])], judge: unqualified };
static TWO: Metal =
    Metal { arms: &[metal::once("two", "tests/jobcase", &[], &[])], judge: unqualified };

/// **The record is one function of the readbacks.** A boot the loop refused, a
/// boot a riding test failed and a boot whose own check failed are each
/// judged, and none adds a row; the one boot with no failure of its own is
/// recorded whole.
pub fn a_boot_with_a_failure_of_its_own_adds_no_row() {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = toyos_tmpdir::TempDir::new("metal-records");
    plant(&dir, "passing", PANEL, BOOTED, None);
    plant(&dir, "failing", PANEL, BOOTED, None);
    plant(&dir, "refused", PANEL, BOOTED, Some(&Refusal::HungWithoutARecord));
    plant(&dir, "late", &expired_at(120_160), &armed(), None);
    let readbacks = read(&dir, &["passing", "failing", "refused", "late"]);
    let why = readbacks["refused"].as_ref().err().expect("the loop's refusal, read back");
    assert!(why.contains("never reported: no panic"), "{why}");

    let tests = [("passes", &PASSING), ("fails", &FAILING), ("refused", &REFUSED), ("late", &LATE)];
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    assert!(metal::judge_readbacks(&root, &readbacks, &runs, &[]), "three failed boots judged green");
    let record = Record::load(&root, &t14()).expect("a readable record").expect("a record");
    let names: Vec<&str> = record.measured.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        [
            "boot.passing.complete_ms",
            "boot.passing.panel_max_us",
            "boot.passing.panel_us",
            "span.passing.us"
        ]
    );
}

/// Two boots under one name are judged on the first reading and recorded off
/// neither.
pub fn a_name_two_boots_measured_is_refused() {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = toyos_tmpdir::TempDir::new("metal-records");
    plant(&dir, "one", PANEL, BOOTED, None);
    plant(&dir, "two", PANEL, BOOTED, None);
    let readbacks = read(&dir, &["one", "two"]);
    let tests = [("one", &ONE), ("two", &TWO)];
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    assert!(metal::judge_readbacks(&root, &readbacks, &runs, &[]), "one name twice judged green");
    let record = Record::load(&root, &t14()).expect("a readable record").expect("a record");
    assert!(!record.measured.contains_key("latency.p99_us"), "{:?}", record.measured);
    assert_eq!(record.measured.len(), 6, "{:?}", record.measured);
}

/// `passing`'s four rows under `bios`: its planted readings, with `complete_ms`
/// as given.
fn committed(bios: &str, complete_ms: u64) -> Record {
    let machine = t14();
    Record {
        vendor: machine.vendor,
        product: machine.product,
        bios: bios.to_string(),
        measured: [
            ("boot.passing.complete_ms", complete_ms),
            ("boot.passing.panel_max_us", 3851),
            ("boot.passing.panel_us", 21751),
            ("span.passing.us", 7),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect(),
    }
}

/// `passing`'s readback judged against `record`, committed first: whether the
/// run was red, and the record it left.
fn judged_against(record: &Record) -> (bool, Record) {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = toyos_tmpdir::TempDir::new("metal-records");
    plant(&dir, "passing", PANEL, BOOTED, None);
    record.save(&root).expect("a committed record");
    let tests = [("passes", &PASSING)];
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    let red = metal::judge_readbacks(&root, &read(&dir, &["passing"]), &runs, &[]);
    (red, Record::load(&root, &t14()).expect("a readable record").expect("a record"))
}

/// **A reading past its record reds the run and moves nothing**: one boot,
/// green against a record it meets and red against one it is past.
pub fn a_reading_past_its_record_fails_and_moves_nothing() {
    let bios = t14().bios;
    let met = committed(&bios, 1165);
    assert_eq!(judged_against(&met), (false, met));
    // 1165 ms against a ceiling of 1164.
    let past = committed(&bios, 582);
    assert_eq!(judged_against(&past), (true, past));
}

/// **A run under another BIOS reds and records nothing**, though every reading
/// meets the record; the same record under this run's own BIOS is green and
/// gains the row it lacks.
pub fn a_run_under_another_bios_fails_and_records_nothing() {
    let lacking = |bios: &str| {
        let mut record = committed(bios, 1165);
        record.measured.remove("span.passing.us");
        record
    };
    let older = lacking("N34ET50W (1.50 )");
    assert_eq!(judged_against(&older), (true, older));
    let bios = t14().bios;
    assert_eq!(judged_against(&lacking(&bios)), (false, committed(&bios, 1165)));
}

/// **A failing shared member fails its boot**: the run is red, and that boot
/// adds no row while the shared boot whose members all passed is recorded
/// whole.
pub fn a_failing_shared_member_fails_its_boot() {
    let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
    let root = toyos_tmpdir::TempDir::new("metal-records");
    let jobs = ["test_rs_std_tls", "test_rs_fs_large_file"];
    let exits = |code: i32| {
        format!(
            "{BOOTED}[2026-09-29 18:22:41 2.310 cpu3] exit: {} pid=12 code=0 cpu=4ms\n\
             [2026-09-29 18:22:42 3.120 cpu5] exit: {} pid=13 code={code} cpu=9ms\n",
            jobs[0], jobs[1]
        )
    };
    plant(&dir, "passing", PANEL, &exits(0), None);
    plant(&dir, "failing", PANEL, &exits(101), None);
    let shared = |boot: &str| metal::SharedBoot {
        boot: boot.to_string(),
        config: "tests/testcases",
        params: &[],
        features: &[],
        members: const { std::num::NonZeroUsize::new(38).expect("a chunk holds a member") },
        jobs: Vec::from(jobs.map(String::from)),
        files: Vec::new(),
        links: Vec::new(),
    };
    let readbacks = read(&dir, &["passing", "failing"]);
    assert!(
        metal::judge_readbacks(&root, &readbacks, &[], &[shared("passing"), shared("failing")]),
        "a failing member judged green"
    );
    let record = Record::load(&root, &t14()).expect("a readable record").expect("a record");
    let names: Vec<&str> = record.measured.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["boot.passing.complete_ms", "boot.passing.panel_max_us", "boot.passing.panel_us"]
    );
}

/// **A page the pass after the reset cleared owes no panel**: `foreignrecord`'s
/// census went with its record, so the boot is green and records its
/// `complete_ms` alone. The same record cleared by the pass before the handoff
/// leaves this boot's own page owing the census, and without it the boot is red
/// and records nothing.
pub fn a_cleared_page_owes_no_panel() {
    let cleared = format!(
        "Black box: 0x8000000 {} ([3e, d4, 0b, d4, 87, ad, 6a, 47, 84, b4, af, c3, f3, 6b, f7, \
         81], and this stick is [c1, d4, 0b, d4, 87, ad, 6a, 47, 84, b4, af, c3, f3, 6b, f7, 81]), \
         armed at 2026-09-29-131341. It has been cleared and this pass boots its kernel\n",
        bootlog::FOREIGN_DONE
    );
    let hung = format!(
        "Boot attempts: this image has had the machine 1 time(s) without reporting; now 0\n{}\n{}\n",
        bootlog::HUNG_WITHOUT_A_RECORD,
        bootlog::CHAIN_ENDS_LINE
    );
    // `before` goes in the pass before the handoff, `page` in the pass after
    // the reset.
    let judged = |before: &str, page: &str| {
        let dir = toyos_tmpdir::TempDir::new("metal-readbacks");
        let root = toyos_tmpdir::TempDir::new("metal-records");
        plant(&dir, "foreignrecord", page, BOOTED, None);
        let at = metal::at(&dir, "foreignrecord").join(READBACK_LOADER);
        let handoff = bootlog::LOADER_LAST_LINE;
        let loader = fs::read_to_string(&at).expect("a planted loader.log");
        fs::write(&at, loader.replacen(handoff, &format!("{before}{handoff}"), 1))
            .expect("a planted loader.log");
        let red = metal::judge_readbacks(&root, &read(&dir, &["foreignrecord"]), &[], &[]);
        let record = Record::load(&root, &t14()).expect("a readable record");
        (red, record.map(|record| record.measured.into_keys().collect::<Vec<_>>()))
    };
    assert_eq!(
        judged("", &format!("{cleared}{hung}")),
        (false, Some(vec!["boot.foreignrecord.complete_ms".to_string()]))
    );
    assert_eq!(judged(&cleared, &hung), (true, None));
}
