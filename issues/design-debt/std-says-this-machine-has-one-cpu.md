---
status: open
kind: defect
opened: 2026-08-23
---

# `std::thread::available_parallelism` answers 1 on every ToyOS machine

`rust/library/std/src/sys/thread/toyos.rs:66` returns a hardcoded
`NonZero::new_unchecked(1)`, and the comment above it says why: "ToyOS runs on
QEMU with a known number of CPUs, but we don't expose a syscall for this yet."
That is no longer true. `SYS_CPU_COUNT` exists, `toyos_abi::syscall::cpu_count`
wraps it, and `userland/libc/src/misc.rs:222` already answers
`_SC_NPROCESSORS_ONLN` from it — so a C program on this system learns the CPU
count and a Rust program is told there is one CPU.

The same file's `yield_now` is a `spin_loop` hint for the same stale reason;
there is no yield syscall, which is a separate decision and not this one.

What it costs: any Rust program that sizes a thread pool, a work-stealing queue
or a shard count from `available_parallelism` runs single-threaded on an eight-
core machine and nothing says so. It bit a test that needed one thread per CPU
(`tests/toyos-rust-tests/src/bin/demand_window_race.rs`, which calls
`syscall::cpu_count` and says why); a test that had *not* noticed would have
gone quietly single-threaded and passed while staging nothing.

`std_threading` asserts only `available_parallelism() > 0`, which a constant 1
satisfies, so nothing in the estate would catch this changing back either.

The fix is one call and touches the fork, so it lands under `src/forkcheck.rs`'s
rules.
