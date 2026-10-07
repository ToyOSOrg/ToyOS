//! Hold the boot open with `/system/bin/acpiserver` serving it, and exit when
//! the runner's own bound is near. It asserts nothing.
//!
//! The `acpi_server_events` metal row runs it: its judge reads a count line the
//! server writes only once one of its count intervals has passed, which a boot
//! that ends with the shared block's jobs does not reach.

use std::thread::sleep;
use std::time::Duration;

const UNTIL_MS: u64 = toyos_tco::JOB_BOUND_MS - toyos_tco::JOB_BOUND_MS / 10;

fn main() {
    let since_boot = toyos_abi::clock::nanos_since_boot() / 1_000_000;
    println!("acpi_hold: holding the boot open to {UNTIL_MS} ms, {since_boot} ms in");
    sleep(Duration::from_millis(UNTIL_MS.saturating_sub(since_boot)));
    println!("acpi_hold: held to {UNTIL_MS} ms, and nothing stopped the machine");
}
