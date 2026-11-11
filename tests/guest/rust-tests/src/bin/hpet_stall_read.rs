//! Wait until the kernel's clock has read the span `hpet-stall` staged and
//! reported once more, and exit non-zero where the runner's own bound comes
//! near first.
//!
//! The `a_stalled_hpet_is_told_by_the_pm_timer` metal row runs it on the boot
//! armed with `hpet-stall`: the kernel stages the stall at its clock's first
//! report, the next sample reads the staged span, and the report after that
//! says the clock went on. The row's judge reads those records, so this holds
//! the boot until they exist and judges nothing.

use std::time::Duration;

#[path = "../served_log.rs"]
mod served_log;

/// The kernel's records, as `kernel/src/arch/x86_64/clocksource.rs` writes
/// them: the stall staged, and a report of the clock's.
const STAGED: &str = "hpet-stall: the HPET reads ";
const REPORT: &str = "clock: over ";

/// How long after boot the wait gives up: the runner's bound less a tenth.
const UNTIL_MS: u64 = toyos_tco::JOB_BOUND_MS - toyos_tco::JOB_BOUND_MS / 10;

fn main() {
    let since_boot = toyos_abi::clock::nanos_since_boot() / 1_000_000;
    let bound = Duration::from_millis(UNTIL_MS.saturating_sub(since_boot));
    let mut staged = false;
    served_log::Log::open().until("the clock's report after the span hpet-stall staged", bound, |line| {
        let kernel = toyos_logstream::parse(line).filter(|said| said.source == toyos_logstream::Source::Kernel);
        let Some(said) = kernel else { return false };
        if said.text.starts_with(STAGED) {
            staged = true;
            return false;
        }
        staged && said.text.starts_with(REPORT)
    });
    println!("hpet_stall_read: the clock read the staged span and reported after it");
}
