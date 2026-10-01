//! A running service's binary replaced with no reboot, rehearsed in QEMU: netd
//! swapped for its own rebuild while `logd` streams to this host and
//! `/system/bin/swap`, run over ssh, carries the ask — and the three ways a swap
//! must leave the old service running.
//!
//! **The machine's own `/log` is the oracle**, read off the image behind the
//! guest's back once `reboot` over ssh has ended the boot: one `Boot:
//! complete` in it is the claim that nothing rebooted, the kernel's `spawn:`
//! record names the binary it loaded, and the stream's lines must be the
//! file's own in its order. What the host heard ([`metalswap::judge`]) is the
//! same reading the T14 run gets.

use toyos_build::bootlog;
use toyos_build::metalswap::{self, Expect};

/// The swapping boot's one job on the T14: it holds the machine until the
/// swap invocation hands it back.
const HOLD: &str = "test_rs_lan_swap_hold";
pub const HOLD_JOBS: &[&str] = &[HOLD];

/// The first line in `file` holding `needle` at or after index `from`.
fn after(file: &[String], from: usize, needle: &str) -> Option<usize> {
    file[from.min(file.len())..].iter().position(|l| l.contains(needle)).map(|at| from + at)
}

/// The T14's swap, judged: what the swap invocation heard over the cable, held
/// against the stick's own `/log` — which came back over a different path and
/// is the oracle for all of it. One `Boot: complete` in the file is the claim
/// that nothing rebooted between the two netds.
pub fn swapped_on_metal(back: &super::metal::Readback) -> Result<(), String> {
    let (swapped, stream) = back.swap()?;
    let mut bad: Vec<String> = Vec::new();
    match metalswap::judge(&swapped, Expect::InService) {
        Ok(said) => said.iter().for_each(|line| eprintln!("  [swap] {line}")),
        Err(found) => bad.extend(found),
    }
    let file: Vec<String> = back.log().text().split_inclusive('\n').map(str::to_string).collect();
    if let Err(why) = super::logstream::is_prefix_of(&stream, &file) {
        bad.push(why);
    }
    let boots = file.iter().filter(|l| l.contains(bootlog::COMPLETE)).count();
    if boots != 1 {
        bad.push(format!("the stick's log holds {boots} `Boot: complete` record(s), where one boot owes one"));
    }
    match toyos_swap::parse_hex(&swapped.digest) {
        Some(digest) => {
            let installed = toyos_swap::installed_path(&swapped.service, &digest);
            match after(&file, 0, &format!("spawn: {installed}")) {
                Some(spawned) => match after(&file, spawned, toyos_build::lan::LEASE) {
                    Some(leased) => eprintln!(
                        "  [swap] the stick: {installed} spawned at line {spawned}, a lease after it at {leased}"
                    ),
                    None => bad.push(format!("the stick's log has no lease after {installed} was spawned")),
                },
                None => bad.push(format!("the stick's log has no `spawn: {installed}` record")),
            }
        }
        None => bad.push(format!("the swap file's digest {:?} is no digest", swapped.digest)),
    }
    // The host said it was done: its `reboot` ended the boot, not the
    // runner's bound over a hold that never ends on its own.
    if file.iter().any(|l| l.contains(toyos_build::bootlog::JOB_DEADLINE_SAID)) {
        bad.push(format!(
            "the runner's bound ended the boot inside {HOLD}, so the swap invocation never \
             handed the machine back"
        ));
    }
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!("{} finding(s):\n  {}", bad.len(), bad.join("\n  ")))
}

