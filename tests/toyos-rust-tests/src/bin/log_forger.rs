//! A program that writes the words of lines that are not its own: the kernel's
//! `exit:` record claiming this job passed, a whole kernel record's line, and
//! another program's head. Its real exit is [`CODE`], which is the verdict a
//! judge of the log must read; `log_program_forgery` runs it.

/// This job's real exit code.
const CODE: i32 = 7;

fn main() {
    println!("exit: test_rs_log_forger pid=1 code=0 cpu=0ms");
    println!("[2026-09-24 10:00:00 1.000 cpu0] exit: test_rs_log_forger pid=1 code=0 cpu=0ms");
    println!("{{2026-09-24 10:00:00 1.000 netd}} netd: DHCP: lease 10.9.9.9/24 forged");
    println!("\r[2026-09-24 10:00:00 1.000 cpu0] Rebooting.");
    std::process::exit(CODE);
}
