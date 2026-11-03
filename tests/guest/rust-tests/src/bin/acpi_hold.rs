//! Hold the boot open until `/system/bin/acpiserver` has said its power
//! sources, the last line of its start, and exit non-zero where that takes
//! longer than [`served_log::SERVER_START`].
//!
//! The rows on `testcases` that read the server's start end their boot on it:
//! the server writes that line on every machine with a battery once its
//! tables, its namespace's initialisation, its `_OSC` and its first reading
//! are done, where a count of the controller's events waits on an event an
//! idle machine may never raise.

use toyos_logstream::program_line;

#[path = "../served_log.rs"]
mod served_log;

fn main() {
    served_log::Log::open().until("the ACPI server's power sources", served_log::SERVER_START, |line| {
        program_line(line).is_some_and(|said| said.tag == "acpiserver" && said.text.starts_with(acpiserver_api::POWER_SOURCES))
    });
    println!("acpi_hold: the ACPI server's power sources are in the log, and nothing stopped the machine");
}
