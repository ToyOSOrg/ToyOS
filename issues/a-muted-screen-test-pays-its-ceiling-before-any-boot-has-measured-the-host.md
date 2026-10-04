---
status: open
kind: tooling
opened: 2026-10-04
---

# A muted screen test pays its ceiling before any boot has measured the host

`QemuInstance::screendump_while` (`tests/common/qemu.rs`) fixes its deadline
once, at its start, through `budget_smp`, whose host factor is the run's
fastest boot so far (`host_scale`). A muted guest reaches no ready marker, so
its own boot never feeds that factor, and a muted test that starts before any
other guest of the run has booted pays its ceiling at 1×, however loaded the
host is.

Seen once, on the dev host at load averages 80.26, 74.24 and 66.53 (14
cores), in a whole `cargo test --test toyos-build` run at `7c550fa1f` on
`wt/toyos-irqon`: `screen_panic_muted` was one of the twelve tests the run
started at 07:17:52, its kernel built at 07:18:20, and it was red at 07:18:51,
before any test of the run had passed, with `"PANIC:" not on
screen of a guest with no serial port at all`. The decoded screen held only
the kernel's first three lines, all stamped 0.000. The same run ended with
`fastest boot 2162 ms against the reference 1424 ms — liveness ceilings paid
at 1.52x`. The whole run at `2b9697284`, at 1.01x, had it green.

**Exit**: a muted screen test's wait is bounded by the guest's progress or by a
host factor measured before its deadline is fixed, and a muted test run first
on a loaded host is green.
