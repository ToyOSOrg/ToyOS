//! Wait until `/system/bin/acpiserver` has logged a count of the embedded
//! controller's queries, and exit non-zero where that takes longer than
//! [`BOUND`] from this job's start.
//!
//! The `acpi_server_events` metal row runs it among its boot's rows' jobs: its
//! judge reads that line, which the server writes only once one of its count
//! intervals has passed in which the controller raised a query, so it is that
//! row's premise alone and holds no other row's boot.

use std::time::Duration;

use toyos_logstream::program_line;

#[path = "../served_log.rs"]
mod served_log;

/// How long the wait gives up after, from this job's own start: the line comes
/// at the end of the count interval in which the controller's first query
/// fell, so a job begun before the server's start waits for that start, the
/// query and at most one interval; [`served_log::START_LINE`] and two
/// intervals are a liveness ceiling past it.
const BOUND: Duration = Duration::from_secs(
    served_log::START_LINE.as_secs() + 2 * acpiserver_api::COUNT_INTERVAL.as_secs(),
);

fn main() {
    served_log::Log::open().until("the ACPI server's count of its controller's queries", BOUND, |line| {
        program_line(line).is_some_and(|said| said.tag == "acpiserver" && said.text.contains(acpiserver_api::QUERIES_COUNTED))
    });
    println!("acpi_counted: the ACPI server's count is in the log, and nothing stopped the machine");
}
