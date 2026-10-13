//! Hold the boot open until `/system/bin/ps2server`'s probe has ended: it has
//! said it armed the i8042, or the kernel has recorded its exit. Exits
//! non-zero where the runner's own bound comes near first.
//!
//! The rows that read what ps2server said end their boot on this: its probe
//! waits on the keyboard's reset and the aux device's, the better part of a
//! second on the T14, and a boot that ends sooner cuts its lines off.

use std::time::Duration;

use toyos_logstream::{parse, program_line, Source};

#[path = "../served_log.rs"]
mod served_log;

/// How long after boot the wait gives up: the runner's bound less a tenth.
const UNTIL_MS: u64 = toyos_tco::JOB_BOUND_MS - toyos_tco::JOB_BOUND_MS / 10;

/// ps2server's line once it armed the controller, whatever it armed.
const ARMED: &str = "ps2server: armed, ";
/// The head of the kernel's record of ps2server's exit.
const EXITED: &str = "exit: ps2server pid=";

fn main() {
    let since_boot = toyos_abi::clock::nanos_since_boot() / 1_000_000;
    let bound = Duration::from_millis(UNTIL_MS.saturating_sub(since_boot));
    let mut ended = String::new();
    served_log::Log::open().until("the end of ps2server's probe", bound, |line| {
        let armed = program_line(line).is_some_and(|said| said.tag == "ps2server" && said.text.starts_with(ARMED));
        let exited = parse(line).is_some_and(|p| p.source == Source::Kernel && p.text.starts_with(EXITED));
        if armed || exited {
            ended = line.trim_end().to_string();
        }
        armed || exited
    });
    println!("ps2server_hold: ps2server's probe ended, and nothing stopped the machine: {ended}");
}
