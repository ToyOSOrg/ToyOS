//! A program that talks and then halts the machine through `SYS_DEBUG`, so a
//! fatal path meets the panel while a program's lines are being painted.
//!
//! With `panel-painter-stalls` armed the kernel holds the fatal halt until one
//! of those repaints is stuck inside the panel's latch.
//! `screen_fatal_behind_a_painter` runs it.

use std::io::Write;

fn main() {
    let mut out = std::io::stdout().lock();
    for i in 0..64 {
        let _ = writeln!(out, "talking {i:03}");
        let _ = out.flush();
    }
    toyos_abi::syscall::debug(toyos_abi::syscall::debug_action::FATAL_HALT);
    eprintln!("ERROR: SYS_DEBUG FATAL_HALT returned");
}
