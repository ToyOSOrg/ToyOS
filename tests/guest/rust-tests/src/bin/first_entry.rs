//! A new thread's first instruction finds zero in every general register but
//! its stack pointer and its argument in `rdi`: whatever else is there is the
//! kernel's, its stack and heap addresses among them. A process's first entry
//! is the same tail with no argument (`arch::entry::first_entry`).

use toyos_abi::syscall::{thread_join, thread_spawn, SYS_THREAD_EXIT};

/// The registers [`probe`] stores, in its order.
const NAMES: [&str; 14] =
    ["rax", "rbx", "rcx", "rdx", "rsi", "rbp", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];

#[repr(C, align(16))]
struct Registers([u64; 14]);

fn main() {
    let mut found = Registers([u64::MAX; 14]);
    let mut stack = vec![0u128; 1024];
    let base = stack.as_mut_ptr() as u64;
    let top = base + (stack.len() * 16) as u64;
    // SAFETY: `probe` touches only the `Registers` its argument names, which
    // outlives the join below, and never its stack.
    let tid = unsafe { thread_spawn(probe as *const () as u64, top, (&raw mut found) as u64, base) };
    assert!(tid < 1_000_000, "thread_spawn refused: {tid:#x}");
    assert_eq!(thread_join(tid), 0, "thread_join failed");
    drop(stack);
    let held: Vec<String> =
        NAMES.iter().zip(found.0).filter(|&(_, v)| v != 0).map(|(name, v)| format!("{name}={v:#x}")).collect();
    assert!(held.is_empty(), "a new thread's first instruction found {}", held.join(" "));
    println!("first_entry: every general register but rsp and rdi was zero at a new thread's first instruction");
}

/// Store every general register but `rsp` and `rdi` at `[rdi]` before any of
/// them is written, and exit the thread.
#[unsafe(naked)]
extern "C" fn probe() {
    core::arch::naked_asm!(
        "mov [rdi + 0], rax",
        "mov [rdi + 8], rbx",
        "mov [rdi + 16], rcx",
        "mov [rdi + 24], rdx",
        "mov [rdi + 32], rsi",
        "mov [rdi + 40], rbp",
        "mov [rdi + 48], r8",
        "mov [rdi + 56], r9",
        "mov [rdi + 64], r10",
        "mov [rdi + 72], r11",
        "mov [rdi + 80], r12",
        "mov [rdi + 88], r13",
        "mov [rdi + 96], r14",
        "mov [rdi + 104], r15",
        "mov edi, {exit}",
        "xor esi, esi",
        "syscall",
        "ud2",
        exit = const SYS_THREAD_EXIT,
    );
}
