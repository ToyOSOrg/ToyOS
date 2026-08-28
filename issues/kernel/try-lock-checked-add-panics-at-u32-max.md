---
status: open
kind: defect
opened: 2026-08-28
---

# try_lock's `current + 1` is a checked add, so a Lock whose `now` reaches u32::MAX panics the kernel at the next try_lock

`Lock::try_lock` computes the next ticket with a plain `u32` add instead of the wrapping atomic the rest of the ticket protocol uses, so when a lock's `now` counter reaches `u32::MAX` the next `try_lock` on it panics the kernel under `overflow-checks`.

## Mechanism (ToyOS terms)

`Lock` (kernel/src/sync.rs:23-27) is a ticket spinlock over two `AtomicU32`s: `ticket` (acquisitions requested) and `now` (acquisitions completed). `now` is advanced only by `LockGuard::drop` via `self.lock.now.fetch_add(1, Release)` (sync.rs:123), and `lock()` advances `ticket` via `self.ticket.fetch_add(1, Relaxed)` (sync.rs:57). Both are atomic RMWs, which wrap unconditionally in Rust regardless of `overflow-checks`, so `lock()`/`unlock()` cycle through the `u32` wrap without incident.

`try_lock` (sync.rs:81-91) is the exception:

```
let current = self.now.load(ACQUIRED);
match self.ticket.compare_exchange(current, current + 1, Ordering::Relaxed, Ordering::Relaxed) {
```

`current + 1` is a plain `u32` add on a local, evaluated as an ordinary arithmetic expression before `compare_exchange` runs. Under `overflow-checks` it is a checked add: when `current == u32::MAX` it panics — and it panics whether or not the CAS would have succeeded, because the argument is computed first. It is the only ticket operation in the file that is not a wrapping atomic; grep of sync.rs for `wrapping_add`/`checked_add`/`saturating_add` returns nothing.

`overflow-checks` is on in the shipping kernel, not a debug-only artifact: `[profile.toyos]` sets `overflow-checks = true` (kernel/Cargo.toml:343-348), and `assert_overflow_checked` (src/build.rs:493-503) refuses to build any kernel image whose bytes lack the overflow-check marker.

## Impact

A panic inside `try_lock` violates two invariants at once. The crash path carries an explicit DESIGN RULE (kernel/src/arch/idt/exceptions.rs:138-142) that everything `crash_report` calls "must stay panic-free ... try_lock only" — `try_lock` is chosen there precisely for panic-freedom, and it is the panic hazard. And "the kernel never crashes from userland": an unprivileged user fault reaches this add.

## File:line chain on current main (19e25d57)

- kernel/src/sync.rs:83-84 — the checked `current + 1` in `try_lock`.
- kernel/src/sync.rs:123 — `now.fetch_add(1, Release)`, the wrapping counter that reaches `u32::MAX`.
- kernel/src/arch/idt/exceptions.rs:191, 298-300 — `theirs = ctx.blame() != Blame::Kernel`; on a userland-blamed fault, `process::dump_crash_diagnostics(...)` is called with no privilege gate.
- kernel/src/process.rs:1486 and 1496 — `PROCESS_TABLE.try_lock()` and the crashing process's `data_arc.try_lock()` on that path.
- kernel/src/process.rs:646 (`try_for_each_thread`) and kernel/src/arch/syscall/machine.rs:146 (ROSTER-gated `sys_sysinfo`) — the other two `try_lock` sites on PROCESS_TABLE/ProcessData.

## Precondition / repro

A single Lock's `now` must equal `u32::MAX` — 2^32 completed acquisitions on that one lock — at the moment a `try_lock` fires on it. PROCESS_TABLE's `now` is fed by ~21 `lock()` sites and 3 `try_lock()` sites across scheduler.rs/process.rs/machine.rs, so it drifts toward the boundary from routine operation over long uptime, independent of attacker intent. On reaching the boundary, any user-mode segfault (which routes through dump_crash_diagnostics -> try_lock) panics the kernel. Deliberately triggering it takes on the order of 2^32 lock cycles on that specific Lock plus a try_lock landing in the narrow u32::MAX window, so it is a latent wear/uptime panic, not a one-shot crafted-input crash.

## Fix direction

Replace `current + 1` with `current.wrapping_add(1)` to match the wrapping semantics of the two atomic RMW ticket ops (sync.rs:57, 123). The compare_exchange still fails correctly when the lock is contended; the change only removes the spurious overflow panic at the wrap boundary. A negative control is a unit test that seeds `now`/`ticket` at `u32::MAX`, calls `try_lock`, and asserts it acquires (and drops back to 0) rather than panicking.
