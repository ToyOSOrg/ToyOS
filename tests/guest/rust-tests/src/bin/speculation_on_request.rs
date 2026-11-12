//! What a program that asked for isolated speculation runs with, read off its
//! CPU's own registers through `SYS_DEBUG`. This job's row does not ask, so it
//! runs without the `IA32_SPEC_CTRL` bits its CPU sets on request;
//! `speculationprobe`'s row asks, so it runs with them, and so does the child
//! it starts itself without asking. Pinned to one CPU beside the probe, this
//! job switches with it between two address spaces one of which asked, and
//! the CPU issues IBPBs, and back on it this job runs without the asked
//! bits again; beside a child of its own that does not ask, none. A CPU that
//! enumerates no IBPB, as TCG's `qemu64` does not, has none of this to hold.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

use std::os::toyos::process::CommandExt;
use toyos_abi::syscall::{self, debug_action, SyscallError, CPU_MASK_WORDS};

const SELF: &str = "/system/bin/test_rs_speculation_on_request";

/// Yields each side of a pair takes on the shared CPU.
const ROUNDS: u32 = 200;

/// How long the other side of a pair may take to end: a failure bound, never
/// what ends a passing run.
const END_BOUND: Duration = Duration::from_secs(30);

/// Whether this CPU enumerates IBPB: Intel's CPUID.(07H,0):EDX[26] or AMD's
/// CPUID Fn8000_0008_EBX[12].
fn enumerates_ibpb() -> bool {
    use core::arch::x86_64::{__cpuid, __cpuid_count};
    let intel = __cpuid(0).eax >= 7 && __cpuid_count(7, 0).edx & (1 << 26) != 0;
    let amd = __cpuid(0x8000_0000).eax >= 0x8000_0008 && __cpuid(0x8000_0008).ebx & (1 << 12) != 0;
    intel || amd
}

fn pin(cpu: u32) {
    let mut mask = [0u64; CPU_MASK_WORDS];
    mask[cpu as usize / 64] = 1 << (cpu % 64);
    syscall::thread_set_affinity(0, &mask).expect("pinned to one CPU");
}

fn answered(action: u64, what: &str) -> u64 {
    let value = syscall::debug(action);
    if let Some(e) = SyscallError::from_u64(value) {
        panic!("{what}: {e}");
    }
    value
}

/// The IBPBs this CPU issued while this job yielded beside `other` until it
/// ended.
fn ibpbs_beside(other: &mut Child) -> u64 {
    let before = answered(debug_action::SWITCH_IBPBS, "this CPU's decision names no IBPB");
    let began = Instant::now();
    loop {
        syscall::yield_now();
        if let Some(status) = other.try_wait().expect("the other side's end") {
            assert!(status.success(), "the other side ended {status:?}");
            break;
        }
        assert!(began.elapsed() < END_BOUND, "the other side did not end within {END_BOUND:?}");
    }
    answered(debug_action::SWITCH_IBPBS, "this CPU's decision names no IBPB") - before
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--beside") {
        for _ in 0..ROUNDS {
            syscall::yield_now();
        }
        return;
    }
    if !enumerates_ibpb() {
        println!("speculation_on_request: this CPU enumerates no IBPB, so nothing here applies");
        return;
    }
    let asked = answered(debug_action::SPEC_CTRL_ON_REQUEST, "no asked bits");
    assert_ne!(asked, 0, "this CPU sets no IA32_SPEC_CTRL bit for a program that asks, so nothing here is held");
    let mine = answered(debug_action::SPEC_CTRL, "this CPU has no IA32_SPEC_CTRL");
    assert_eq!(mine & asked, 0, "a program whose row does not ask runs with IA32_SPEC_CTRL {mine:#x}, and {asked:#x} is asked");

    let cpu = syscall::current_cpu();
    pin(cpu);
    let mut probe = Command::new("/system/bin/speculationprobe")
        .arg(cpu.to_string())
        .arg(ROUNDS.to_string())
        .independent()
        .spawn()
        .expect("the probe launches: testcases' test-runner starts it");
    let beside_probe = ibpbs_beside(&mut probe);
    assert!(beside_probe >= 2, "CPU {cpu} issued {beside_probe} IBPBs while a program that asked took turns with this one there");
    let after = answered(debug_action::SPEC_CTRL, "this CPU has no IA32_SPEC_CTRL");
    assert_eq!(after & asked, 0, "back on CPU {cpu} after a program that asked, this one runs with IA32_SPEC_CTRL {after:#x}");

    // A helper starts with this thread's mask, so it shares the CPU.
    let mut peer = Command::new(SELF).arg("--beside").spawn().expect("a child that does not ask starts");
    let beside_peer = ibpbs_beside(&mut peer);
    assert_eq!(beside_peer, 0, "CPU {cpu} issued {beside_peer} IBPBs between two programs neither of which asked");

    println!(
        "speculation_on_request: IA32_SPEC_CTRL {asked:#x} asked, set for the probe and its child and clear here; \
         {beside_probe} IBPBs on CPU {cpu} beside the probe and none beside a child that did not ask"
    );
}
