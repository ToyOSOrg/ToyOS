//! Hold the boot open until `/system/bin/acpiserver` has ended its start
//! (`acpiserver_api::STARTED`), and exit non-zero where that takes longer than
//! [`served_log::START_LINE`].
//!
//! The rows on `testcases` that read the server's start end their boot on it:
//! the server writes that line on every machine, after its tables, namespace,
//! idle states, GPEs and power sources, where a count of the controller's
//! events waits on an event an idle machine may never raise.

use toyos_logstream::program_line;

#[path = "../served_log.rs"]
mod served_log;

fn main() {
    served_log::Log::open().until("the end of the ACPI server's start", served_log::START_LINE, |line| {
        program_line(line).is_some_and(|said| said.tag == "acpiserver" && said.text == acpiserver_api::STARTED)
    });
    println!("acpi_hold: the ACPI server's start is in the log, and nothing stopped the machine");
}
