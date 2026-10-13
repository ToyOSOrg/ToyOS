//! Hold the boot open until `/system/bin/acpiserver` has said its power
//! sources, the last line of its start, and exit non-zero where the runner's
//! own bound comes near first.
//!
//! The rows on `testcases` that read the server's start end their boot on it:
//! the server writes that line on every machine with a battery once its
//! tables, its namespace's initialisation, its `_OSC` and its first reading
//! are done, where a count of the controller's events waits on an event an
//! idle machine may never raise.

use std::time::Duration;

use toyos_logstream::program_line;

#[path = "../served_log.rs"]
mod served_log;

/// How long after boot the wait gives up: the runner's bound less a tenth.
const UNTIL_MS: u64 = toyos_tco::JOB_BOUND_MS - toyos_tco::JOB_BOUND_MS / 10;

fn main() {
    let since_boot = toyos_abi::clock::nanos_since_boot() / 1_000_000;
    let bound = Duration::from_millis(UNTIL_MS.saturating_sub(since_boot));
    served_log::Log::open().until("the ACPI server's power sources", bound, |line| {
        program_line(line).is_some_and(|said| said.tag == "acpiserver" && said.text.starts_with(acpiserver_api::POWER_SOURCES))
    });
    println!("acpi_hold: the ACPI server's power sources are in the log, and nothing stopped the machine");
}
