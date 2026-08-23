---
status: open
kind: finding
opened: 2026-08-23
---

# `syscall_window_nmi` went red under a parallel suite, green alone

First recorded sighting, 2026-08-23, in a full `cargo test --test toyos-build`
on `wt/toyos-cleanup-syscall`. The branch's whole kernel delta is a
behaviour-preserving refactor of `kernel/src/arch/syscall.rs` — a `demand_syscap`
helper over six SysCap arms and a `resolve_for_modify`/`resolve_and_check` pair
over the mutating filesystem prologue — which touches neither NMI delivery nor
the syscall entry/exit path the test exercises.

Wide phase, 85 guests, host at 1.01x reference width:

```
FAIL syscall_window_nmi: 2905 NMIs were sent and only 1973 taken — the victim
stopped taking them, which is what an NMI that ends a CPU looks like from here
```

The harness re-ran it alone in the same process: `PASS` with
`3000 sent, 3000 taken, 60 in the window, 179 in Ring 3, 888 syscalls made
under the storm`, and printed `ALONE syscall_window_nmi: GREEN — it fails only
beside other guests, so its Sched::Parallel is wrong`.

`cargo run -- --known-red syscall_window_nmi` answers `NOT ON THE LIST`, so this
is its first recorded rate. This is the class
`issues/build/parallel-tests-red-under-other-suites.md` registers, and its shape
matches that file's `dump_nmi_probe` entry (an NMI-delivery verdict expiring
under other worktrees' load); by that file's own doctrine an `ALONE: GREEN` is
the host and not the kernel, because a green cannot be produced by load. Not
investigated further, and not confirmed against `main` in the same session —
which is what the register asks for before an `ALONE: red again` is believed,
and is the next step if this recurs.
