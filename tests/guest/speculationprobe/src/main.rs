//! A program whose row asks for isolated speculation, launched by
//! `speculation_on_request`. Started `<cpu> <rounds>`, it reads its CPU's
//! `IA32_SPEC_CTRL` holding every bit the CPU sets on request, starts itself
//! again without asking, `--child`, which reads the same, and then yields
//! `<rounds>` times pinned to `<cpu>`. Each refusal is its exit.

use std::process::Command;

use toyos_abi::syscall::{self, debug_action, SyscallError, CPU_MASK_WORDS};

const SELF: &str = "/system/bin/speculationprobe";

/// `SYS_DEBUG`'s answer to `action`, which is refused an error word.
fn answered(action: u64) -> u64 {
    let value = syscall::debug(action);
    if let Some(e) = SyscallError::from_u64(value) {
        panic!("speculationprobe: SYS_DEBUG action {action} answered {e}");
    }
    value
}

fn holds_the_asked_bits(who: &str) {
    let asked = answered(debug_action::SPEC_CTRL_ON_REQUEST);
    let now = answered(debug_action::SPEC_CTRL);
    assert!(
        asked != 0 && now & asked == asked,
        "speculationprobe: {who} runs with IA32_SPEC_CTRL {now:#x}, and its CPU sets {asked:#x} for a program that asked"
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next().expect("speculationprobe: `<cpu> <rounds>` or `--child`");
    if first == "--child" {
        return holds_the_asked_bits("the child it started without asking");
    }
    holds_the_asked_bits("the program whose row asked");
    let status = Command::new(SELF).arg("--child").status().expect("speculationprobe: its child starts");
    assert!(status.success(), "speculationprobe: its child ended {status:?}");
    let cpu: usize = first.parse().expect("speculationprobe: a CPU number");
    let rounds: u32 = args.next().and_then(|r| r.parse().ok()).expect("speculationprobe: a count of rounds");
    let mut mask = [0u64; CPU_MASK_WORDS];
    mask[cpu / 64] = 1 << (cpu % 64);
    syscall::thread_set_affinity(0, &mask).expect("speculationprobe: pinned to the job's CPU");
    for _ in 0..rounds {
        syscall::yield_now();
    }
    println!("speculationprobe: it and the child it started without asking ran with the bits asked");
}
