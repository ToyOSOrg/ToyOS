use std::collections::BTreeMap;

use super::*;

/// A row the T14's sampler wrote during a stage-3 build, before rows carried
/// a `read` tag.
const T14_ROW: &str = concat!(
    "utc=2026-09-28T18:20:04.802Z ",
    "epoch=1790619604 ",
    "phase=s3-1 ",
    "temp_mc=70000 ",
    "disk_rd_sectors=nvme0n1:1918110,sda:78852 ",
    "ac=1 ",
    "platform_profile=performance ",
    "rapl_msr=64000000,64000000,121000000,27983872,2440 ",
    "rapl_mmio=20000000,64000000,121000000,27983872,2440 ",
    "msr_610=0042820000dd8200 ",
    "mmio_59a0=0042820000dd80a0 ",
    "msr_601=00000000000003c8 ",
    "msr_770=0000000000000001,0000000000000001,0000000000000001,0000000000000001,0000000000000001,0000000000000001,0000000000000001,0000000000000001 ",
    "msr_774=0000000080002a04,0000000080002a04,0000000080002a04,0000000080002a04,0000000080002a04,0000000080002a04,0000000080002a04,0000000080002a04 ",
    "msr_1b0=0000000000000006,0000000000000006,0000000000000006,0000000000000006,0000000000000006,0000000000000006,0000000000000006,0000000000000006 ",
    "msr_1a0_b38=0,0,0,0,0,0,0,0 ",
    "msr_772=000000008000ff01 ",
    "msr_8b=000000be00000000,000000be00000000,000000be00000000,000000be00000000,000000be00000000,000000be00000000,000000be00000000,000000be00000000 ",
    "governor=powersave,powersave,powersave,powersave,powersave,powersave,powersave,powersave ",
    "epp=balance_performance,balance_performance,balance_performance,balance_performance,balance_performance,balance_performance,balance_performance,balance_performance ",
    "min_khz=400000,400000,400000,400000,400000,400000,400000,400000 ",
    "max_khz=4200000,4200000,4200000,4200000,4200000,4200000,4200000,4200000 ",
    "pstate=active ",
    "no_turbo=0 ",
    "hwp_dynamic_boost=0 ",
    "energy_uj=107418320833",
);
const T14_UTC: &str = "utc=2026-09-28T18:20:04.802Z";

const VERSION: &str = "Linux version 6.8.0-142-generic (fixture) #142-Ubuntu SMP";

/// GNU `time -v`'s output for a ninja that ran `wall`.
fn time_v(wall: &str, exit: u32, inputs: u32) -> String {
    format!(
        "\tCommand being timed: \"ninja -C s3 -j8 clang lld\"\n\
         \tUser time (seconds): 19403.20\n\
         \tPercent of CPU this job got: 795%\n\
         \tElapsed (wall clock) time (h:mm:ss or m:ss): {wall}\n\
         \tMajor (requiring I/O) page faults: 417\n\
         \tFile system inputs: {inputs}\n\
         \tFile system outputs: 4145776\n\
         \tPage size (bytes): 4096\n\
         \tExit status: {exit}\n"
    )
}

fn vulnerabilities() -> String {
    [
        "gather_data_sampling:Mitigation: fixture",
        "spectre_v1:Mitigation: fixture",
        "meltdown:Not affected",
    ]
    .iter()
    .map(|l| format!("{VULNERABILITY}{l}\n"))
    .collect()
}

fn microcodes() -> String {
    format!("microcode\t: {MICROCODE:#x}\n").repeat(CPUS)
}

fn machine(bios: &str, boot: &str) -> String {
    format!(
        "{VERSION}\nBOOT_IMAGE=/vmlinuz-{KERNEL} ro\n{}{}{BIOS}{bios}\n{BOOT}{boot}\n",
        vulnerabilities(),
        microcodes()
    )
}

/// S0's text: its lines among others it prints.
fn s0_text() -> String {
    format!("Reading package lists...\n{VERSION}\nBOOT_IMAGE=/vmlinuz-{KERNEL} ro\n{}{}0x0000000000000000\n", vulnerabilities(), microcodes())
}

fn s0() -> S0 {
    S0::parse(&s0_text()).expect("the fixture's S0 parses")
}

/// The T14's row at `t` seconds into its day, for `span`, tagged `read`.
fn row(span: &str, t: u64, read: &str) -> String {
    let stamp = format!(
        "utc=2026-09-28T{:02}:{:02}:{:02}.500Z",
        t / 3600,
        t / 60 % 60,
        t % 60
    );
    T14_ROW.replacen(T14_UTC, &stamp, 1).replacen(
        "phase=s3-1 ",
        &format!("phase={span} read={read} "),
        1,
    )
}

/// A span's sampler log: a start read at `t`, timed reads 60 s apart, and an
/// end read 60 s after the last.
fn log(span: &str, t: u64, timed: u64) -> String {
    let mut rows = vec![row(span, t, "start")];
    rows.extend((1..=timed).map(|i| row(span, t + 60 * i, "timed")));
    rows.push(row(span, t + 60 * (timed + 1), "end"));
    rows.join("\n") + "\n"
}

/// A run of s3-0 to s3-3 in which every span is valid.
fn run() -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    for (k, wall) in ["42:37.88", "42:40.00", "42:36.03", "42:38.50"]
        .iter()
        .enumerate()
    {
        let span = format!("s3-{k}");
        files.insert(
            format!("samples-{span}.log"),
            log(&span, 3600 + 1000 * k as u64, 4),
        );
        files.insert(format!("{span}-time.txt"), time_v(wall, 0, 0));
        files.insert(
            format!("{span}-machine-start.txt"),
            machine("BIOS-A", "boot-a"),
        );
        files.insert(
            format!("{span}-machine-end.txt"),
            machine("BIOS-A", "boot-a"),
        );
    }
    files
}

fn judged(files: &BTreeMap<String, String>) -> Run {
    judge(&|name: &str| files.get(name).cloned(), &s0())
}

fn edit(files: &mut BTreeMap<String, String>, name: &str, f: impl FnOnce(&str) -> String) {
    let text = files
        .get(name)
        .unwrap_or_else(|| panic!("the fixture has no {name}"));
    let edited = f(text);
    assert_ne!(&edited, text, "the edit to {name} changed nothing");
    files.insert(name.into(), edited);
}

fn without_line(text: &str, which: fn(usize) -> usize) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    lines.remove(which(lines.len()));
    lines.join("\n") + "\n"
}

/// The one span `edit` turned invalid, and what the judge said.
fn only_refused(files: &BTreeMap<String, String>) -> (String, Vec<String>) {
    let run = judged(files);
    let refused: Vec<&Verdict> = run.verdicts.iter().filter(|v| !v.valid()).collect();
    let [v] = refused[..] else {
        panic!(
            "{} spans refused, not 1: {:#?}",
            refused.len(),
            run.verdicts
        )
    };
    assert_eq!(run.bar_cs, None, "two valid samples set no bar");
    (v.span.clone(), v.refusals.clone())
}

fn says(refusals: &[String], words: &str) -> bool {
    refusals.iter().any(|r| r.contains(words))
}

#[test]
fn a_complete_run_sets_the_bar_to_its_shortest_valid_wall() {
    let run = judged(&run());
    let spans: Vec<(&str, bool)> = run
        .verdicts
        .iter()
        .map(|v| (v.span.as_str(), v.valid()))
        .collect();
    assert_eq!(
        spans,
        [("s3-1", true), ("s3-2", true), ("s3-3", true)],
        "{:#?}",
        run.verdicts
    );
    assert_eq!(run.bar_cs.map(show_wall).as_deref(), Some("42:36.03"));
}

#[test]
fn a_span_without_its_start_read_is_refused() {
    let mut files = run();
    edit(&mut files, "samples-s3-2.log", |t| without_line(t, |_| 0));
    let (span, why) = only_refused(&files);
    assert_eq!(span, "s3-2");
    assert!(says(&why, "row 1: read=timed, not read=start"), "{why:#?}");
}

#[test]
fn a_span_without_its_end_read_is_refused() {
    let mut files = run();
    edit(&mut files, "samples-s3-3.log", |t| {
        without_line(t, |n| n - 1)
    });
    let (span, why) = only_refused(&files);
    assert_eq!(span, "s3-3");
    assert!(says(&why, "read=timed, not read=end"), "{why:#?}");
}

#[test]
fn a_span_cut_off_mid_build_is_refused() {
    let mut files = run();
    edit(&mut files, "samples-s3-3.log", |t| {
        without_line(t, |n| n - 1)
    });
    files.remove("s3-3-time.txt");
    files.remove("s3-3-machine-end.txt");
    let (span, why) = only_refused(&files);
    assert_eq!(span, "s3-3");
    assert!(
        says(&why, "did not finish") && says(&why, "no s3-3-machine-end.txt"),
        "{why:#?}"
    );
}

#[test]
fn a_missing_timed_read_is_a_gap() {
    let mut files = run();
    edit(&mut files, "samples-s3-1.log", |t| without_line(t, |_| 2));
    let (span, why) = only_refused(&files);
    assert_eq!(span, "s3-1");
    assert!(says(&why, "120000 ms after the read before it"), "{why:#?}");
}

#[test]
fn one_element_off_on_one_cpu_is_refused() {
    let mut files = run();
    edit(&mut files, "samples-s3-1.log", |t| {
        t.replacen(
            "0000000080002a04,0000000080002a04,0000000080002a04,0000000080002a04",
            "0000000080002a04,0000000080002a04,0000000080002a04,0000000080002a05",
            1,
        )
    });
    let (_, why) = only_refused(&files);
    assert!(says(&why, "row 1: msr_774="), "{why:#?}");
}

#[test]
fn another_microcode_in_one_row_is_refused() {
    let mut files = run();
    edit(&mut files, "samples-s3-2.log", |t| {
        t.replacen("msr_8b=000000be00000000", "msr_8b=000000bc00000000", 1)
    });
    let (_, why) = only_refused(&files);
    assert!(says(&why, "row 1: msr_8b="), "{why:#?}");
}

#[test]
fn a_build_that_failed_or_read_a_block_is_refused() {
    for (exit, inputs, words) in [(1, 0, "Exit status 1"), (0, 8, "File system inputs 8")] {
        let mut files = run();
        edit(&mut files, "s3-3-time.txt", |_| {
            time_v("42:36.03", exit, inputs)
        });
        let (span, why) = only_refused(&files);
        assert_eq!(span, "s3-3");
        assert!(says(&why, words), "{why:#?}");
    }
}

#[test]
fn a_missing_machine_read_is_refused() {
    for end in ["start", "end"] {
        let mut files = run();
        files.remove(&format!("s3-2-machine-{end}.txt"));
        let (_, why) = only_refused(&files);
        assert!(
            says(&why, &format!("no s3-2-machine-{end}.txt")),
            "{why:#?}"
        );
    }
}

#[test]
fn the_machine_reads_agree_with_each_other_and_with_s0() {
    type Change = fn(&str) -> String;
    let cases: [(&str, Change, &str); 5] = [
        (
            "s3-1-machine-end.txt",
            |_| machine("BIOS-B", "boot-a"),
            "the BIOS version changed",
        ),
        (
            "s3-1-machine-end.txt",
            |_| machine("BIOS-A", "boot-b"),
            "the boot changed",
        ),
        (
            "s3-1-machine-start.txt",
            |t| t.replacen("spectre_v1:Mitigation: fixture", "spectre_v1:Vulnerable", 1),
            "vulnerabilities lines are not S0's",
        ),
        (
            "s3-1-machine-start.txt",
            |t| {
                t.replacen(
                    "6.8.0-142-generic (fixture)",
                    "6.8.0-143-generic (fixture)",
                    1,
                )
            },
            "the kernel is not 6.8.0-142-generic",
        ),
        (
            "s3-1-machine-end.txt",
            |t| t.replacen(": 0xbe", ": 0xbc", 1),
            "microcode lines are not S0's",
        ),
    ];
    for (name, change, words) in cases {
        let mut files = run();
        edit(&mut files, name, change);
        let (span, why) = only_refused(&files);
        assert_eq!(span, "s3-1");
        assert!(says(&why, words), "{name}: {why:#?}");
    }
}

#[test]
fn a_span_after_a_failed_build_is_not_warm() {
    let mut files = run();
    edit(&mut files, "s3-0-time.txt", |_| time_v("42:37.88", 1, 0));
    let (span, why) = only_refused(&files);
    assert_eq!(span, "s3-1");
    assert!(says(&why, "not warm: s3-0"), "{why:#?}");
}

#[test]
fn a_span_after_a_span_without_its_end_read_is_not_warm() {
    let mut files = run();
    edit(&mut files, "samples-s3-1.log", |t| {
        without_line(t, |n| n - 1)
    });
    let run = judged(&files);
    let refused: Vec<&str> = run
        .verdicts
        .iter()
        .filter(|v| !v.valid())
        .map(|v| v.span.as_str())
        .collect();
    assert_eq!(refused, ["s3-1", "s3-2"]);
    assert!(
        says(&run.verdicts[1].refusals, "not warm: s3-1"),
        "{:#?}",
        run.verdicts[1]
    );
}

#[test]
fn the_t14s_own_rows_read_the_envelope() {
    let tagged = |t, read| row("s3-1", t, read);
    let log = [tagged(3600, "start"), tagged(3660, "end")].join("\n");
    let mut refusals = Vec::new();
    assert!(check_log(&log, "s3-1", &mut refusals).is_some());
    assert_eq!(refusals, Vec::<String>::new());

    let untagged = check_log(&[T14_ROW, T14_ROW].join("\n"), "s3-1", &mut refusals);
    assert!(
        untagged.is_none() && says(&refusals, "read=<none>, not read=start"),
        "{refusals:#?}"
    );
}

#[test]
fn a_utc_stamp_reads_as_date_does() {
    let f = fields(T14_ROW).expect("the T14's row is key=value");
    let epoch: u64 = f["epoch"].parse().expect("epoch is a number");
    assert_eq!(utc_ms(f["utc"]).map(|ms| ms / 1000), Ok(epoch));
    assert_eq!(utc_ms("1970-01-01T00:00:00.000Z"), Ok(0));
    assert_eq!(utc_ms("2000-03-01T00:00:00.001Z"), Ok(951_868_800_001));
    assert!(utc_ms("2026-09-28 18:20:04.802Z").is_err());
}

#[test]
fn a_wall_clock_reads_both_of_gnu_times_forms() {
    assert_eq!(wall_cs("42:36.03"), Ok(255_603));
    assert_eq!(wall_cs("1:02:03.45"), Ok(372_345));
    assert_eq!(show_wall(372_345), "1:02:03.45");
    for bad in ["42:36", "42:61.00", "42:36.3", "x:36.03"] {
        assert!(wall_cs(bad).is_err(), "{bad}");
    }
}

#[test]
fn s0_is_refused_without_one_version_or_every_cpus_microcode() {
    assert!(S0::parse(&format!("{}{VERSION}\n", s0_text())).is_err());
    assert!(S0::parse(&s0_text().replacen("microcode\t: 0xbe\n", "", 1)).is_err());
    assert!(S0::parse(&s0_text().replace(VULNERABILITY, "/elsewhere/")).is_err());
}

#[test]
fn the_sampler_writes_every_key_the_judge_reads() {
    let sampler = include_str!("../t14/sampler.sh");
    for key in wanted()
        .iter()
        .map(|(k, _)| *k)
        .chain(["utc", "phase", "read"])
    {
        assert!(
            sampler.contains(&format!(" {key}=$")) || sampler.contains(&format!("\"{key}=$")),
            "sampler.sh writes no {key}"
        );
    }
}
