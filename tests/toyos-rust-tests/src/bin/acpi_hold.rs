//! Hold the boot open until `/system/bin/acpiserver` has logged a count of the
//! embedded controller's queries, and exit non-zero where the runner's own
//! bound comes near first.
//!
//! The `acpi_server_events` metal row runs it, last in its boot: its judge
//! reads that line, which the server writes only once one of its count
//! intervals has passed, and a boot whose other jobs end sooner does not reach
//! it.

use std::time::Duration;

use toyos_logstream::program_line;

#[path = "../served_log.rs"]
mod served_log;

/// How long after boot the wait gives up: the runner's bound less a tenth.
const UNTIL_MS: u64 = toyos_tco::JOB_BOUND_MS - toyos_tco::JOB_BOUND_MS / 10;

/// The server's count line, after its count of SCIs.
const COUNTS: &str = "embedded controller queries taken: ";

fn main() {
    let since_boot = toyos_abi::clock::nanos_since_boot() / 1_000_000;
    let bound = Duration::from_millis(UNTIL_MS.saturating_sub(since_boot));
    served_log::Log::open().until("the ACPI server's count of its controller's queries", bound, |line| {
        program_line(line).is_some_and(|said| said.tag == "acpiserver" && said.text.contains(COUNTS))
    });
    println!("acpi_hold: the ACPI server's count is in the log, and nothing stopped the machine");
}
