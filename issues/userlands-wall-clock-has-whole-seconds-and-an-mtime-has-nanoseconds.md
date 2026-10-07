---
status: open
kind: defect
opened: 2026-09-28
---

# Userland's wall clock has whole seconds and a file's mtime has nanoseconds

The kernel stamps a file with `clock::mtime_now` (`kernel/src/clock.rs`), the
RTC's second carried on by the counter, so a write inside a second carries the
nanoseconds past it. Userland reads the wall clock only through
`SYS_CLOCK_EPOCH`, which answers whole seconds: std's `SystemTime::now` (`sdk/std/sys/time.rs`) and libc's `clock_gettime(CLOCK_REALTIME)`
and `gettimeofday` (`userland/libc/src/time.rs`) all round down to the second.

So a file written a moment ago reads as up to a second in the future against
the program's own "now", which a tool comparing at nanoseconds — GNU make's
"modification time in the future" check — reports as clock skew.

**Exit condition.** Userland reads the wall clock at the resolution the kernel
stamps at — the boot's UTC anchor published where a process reads the
monotonic clock (`toyos_abi::clock`'s page), or a syscall that answers
nanoseconds — and std, libc and every file server stamp and compare off it.
