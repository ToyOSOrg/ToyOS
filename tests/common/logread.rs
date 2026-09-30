//! `SYS_LOG_READ`, read from inside `test-runner` under a storm.
//!
//! **The verdict is computed in the guest and asserted here.** What the host
//! can see of a conservation law is a line saying it held; what it can check is
//! that the line is there, that the run was not vacuous, and that the numbers
//! the guest printed describe the machine the host booted. So the guest prints
//! its ledger and this file reads it — `log-gate: OK` is the verdict, and every
//! number beside it is evidence a reviewer can weigh.
//!
//! The gate runs *inside* `test-runner` rather than in a binary it spawns:
//! `logread` is a `SysCap` dup and not a namespace entry, so it is not part of
//! what the runner hands its children.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use super::qemu::{BootOptions, QemuInstance};

/// The in-guest gate's name in the `run <name>` protocol. It is a `test-runner`
/// builtin rather than a `/system/bin` entry, and the marker protocol is the same
/// either way.
const GATE: &str = "log-gate";

/// The same gate with its own producer thread storming the log beside it.
const STORM_GATE: &str = "log-storm";

/// The whole run's ceiling: a gate that never finishes is what it reds.
const CEILING: Duration = Duration::from_secs(60);

/// One boot's storm, as the guest reported it.
struct Report {
    stdout: String,
    fields: BTreeMap<String, u64>,
}

impl Report {
    fn get(&self, key: &str) -> Result<u64, String> {
        self.fields
            .get(key)
            .copied()
            .ok_or_else(|| format!("the guest's report has no `{key}=`:\n{}", self.stdout))
    }
}

/// A name two of the guest's lines both defined.
///
/// **Not a merge, because the two lines are different subjects.** The guest
/// prints its ledger over several `log-gate:` lines and this file reads them
/// into one map, so a name appearing twice means the number a test asserts on
/// came from whichever line was printed last — silently, and with the other
/// line still on screen looking like the evidence. The nest and storm lines
/// already share `read=` and `dropped=`, and every gate here reads exactly one
/// of the two.
struct Contaminated {
    key: String,
    first: u64,
    second: u64,
}

/// The conservation law, at one width.
fn conservation(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    smp: u32,
) -> Result<(), String> {
    // The test kernel by build rather than by actuator: the producer's
    // `SYS_DEBUG` is what needs it, and nothing is armed.
    let options =
        BootOptions { smp, kernel_features: toyos_build::build::TEST_KERNEL, ..Default::default() };
    let report = storm(test_config, c_bins, rust_bins, STORM_GATE, options)?;
    let shards = report.get("shards")?;
    if shards != smp as u64 {
        return Err(format!(
            "--smp {smp} answered {shards} shard(s); the cursor's shard count is the machine's \
             CPU count\n{}",
            report.stdout
        ));
    }
    // Non-vacuity, and it is the half a green law cannot supply: a reader that
    // took every record after the storm had ended has proved nothing about
    // concurrent producers, and one the ring never lapped has proved nothing
    // about `lost`.
    let concurrent = report.get("concurrent")?;
    let dropped = report.get("dropped")?;
    let read = report.get("read")?;
    let lost = report.get("lost")?;
    if concurrent == 0 || read == 0 || lost == 0 {
        return Err(format!(
            "--smp {smp} read {read} record(s), {concurrent} of them while the storm ran, and \
             lost {lost}\n{}",
            report.stdout
        ));
    }
    eprintln!(
        "  [log] smp={smp}: emitted={} read={read} dropped={dropped} concurrent={concurrent} \
         lost={lost} wakes={}",
        report.get("emitted")?,
        report.get("wakes")?,
    );
    Ok(())
}

/// **`--smp 2`**, so the producer thread has a CPU the reader is not on.
pub fn log_conservation_smp2(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    conservation(test_config, c_bins, rust_bins, 2)
}

/// The nested-`emit` gate: an interrupt that logs, inside another `emit`, on one CPU.
///
/// **The one case loom cannot express and the host cannot stage.** The
/// stimulus is a self-IPI sent from inside a record's own body copy, inside
/// `SYS_LOG_READ` with `IF` opened for it — where `emit`'s IF-off bracket is the
/// only thing holding the interrupt off. The handler emits exactly one shard generation of
/// patterned records; the outer record is then dropped by the ring's own
/// drop-oldest policy, which is what makes "the burst laps the shard" a
/// statement with an arithmetic behind it.
///
/// What is asserted is the conservation ledger over a workload of that shape: every
/// sequence number read or counted lost, every burst record's text regenerated
/// byte for byte from the two numbers it declares, and the burst's own `done`
/// read — so a run in which nothing was injected cannot pass quietly.
///
/// **`--smp 1`, and that is the test's own claim.** Nesting is a property of
/// one CPU: a second CPU adds records to the merge and takes nothing away from
/// what this asks, while at one the interrupted writer and its interrupting
/// handler are provably the same CPU.
pub fn log_nested_emit(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions { smp: 1, kernel_params: &["log-nested-emit"], ..Default::default() };
    let report = storm(test_config, c_bins, rust_bins, GATE, options)?;
    let declared = report.get("declared")?;
    let read = report.get("read")?;
    if read == 0 {
        return Err(format!("the burst was declared and none of it read\n{}", report.stdout));
    }
    eprintln!(
        "  [log] nested: burst declared={declared} read={read} dropped={}",
        report.get("dropped")?
    );
    Ok(())
}

/// Boot one machine as `options` says, run `gate` on it and read its verdict off it.
fn storm(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    gate: &str,
    options: BootOptions,
) -> Result<Report, String> {
    let (smp, params) = (options.smp, options.kernel_params);
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    let result = qemu.run_test(gate, CEILING);
    if let Some(err) = &result.error {
        return Err(format!(
            "--smp {smp} {params:?}: {err}\nstdout:\n{}\nserial tail:\n{}",
            result.stdout,
            tail(&result.serial)
        ));
    }
    match result.exit_code {
        Some(0) => {}
        Some(code) => {
            return Err(format!(
                "--smp {smp} {params:?}: the log gate exited {code}\n{}",
                result.stdout
            ))
        }
        None => {
            return Err(format!("--smp {smp} {params:?}: no exit code\n{}", result.stdout))
        }
    }
    if !result.stdout.contains("log-gate: OK") {
        return Err(format!(
            "--smp {smp} {params:?}: the gate exited 0 without saying so\n{}",
            result.stdout
        ));
    }
    let fields = fields(&result.stdout).map_err(|c| {
        format!(
            "--smp {smp} {params:?}: two of the guest's `log-gate:` lines define `{}` ({} and \
             {}), so every number read out of this report is whichever line came last\n{}",
            c.key, c.first, c.second, result.stdout
        )
    })?;
    Ok(Report { fields, stdout: result.stdout })
}

/// Every `key=<number>` the guest printed, and the two counts it prints as
/// prose. One parse, so a test asserts on a name rather than on a column.
///
/// **A name defined twice is refused rather than merged.** The guest's report is
/// several lines about different subjects, and flattening them means a repeated
/// name silently resolves to the last line printed — with the other line still
/// in the failure message, looking like the evidence. Refusing is what makes the
/// flattening safe: it holds exactly while the names really are unique.
fn fields(stdout: &str) -> Result<BTreeMap<String, u64>, Contaminated> {
    fn put(
        out: &mut BTreeMap<String, u64>,
        key: &str,
        value: u64,
    ) -> Result<(), Contaminated> {
        match out.insert(key.to_string(), value) {
            None => Ok(()),
            Some(first) => Err(Contaminated { key: key.to_string(), first, second: value }),
        }
    }

    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for line in stdout.lines() {
        let Some(rest) = line.split_once("log-gate: ").map(|(_, r)| r) else { continue };
        for word in rest.split_whitespace() {
            let Some((key, value)) = word.split_once('=') else { continue };
            if let Ok(n) = value.trim_end_matches(&[',', ';'][..]).parse::<u64>() {
                put(&mut out, key, n)?;
            }
        }
        // "N record(s) over M read(s) from S shard(s)" — the shape of the line
        // rather than a key, because those three are what the sentence is.
        let words: Vec<&str> = rest.split_whitespace().collect();
        for pair in words.windows(2) {
            let Ok(n) = pair[0].parse::<u64>() else { continue };
            match pair[1] {
                "record(s)" => put(&mut out, "records", n)?,
                "read(s)" => put(&mut out, "reads", n)?,
                "shard(s);" | "shard(s)" => put(&mut out, "shards", n)?,
                _ => {}
            }
        }
    }
    Ok(out)
}

/// The last of a capture, for a failure message. A storm puts thousands of
/// lines on the console and the interesting end is the recent one.
fn tail(serial: &str) -> String {
    let lines: Vec<&str> = serial.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
}
