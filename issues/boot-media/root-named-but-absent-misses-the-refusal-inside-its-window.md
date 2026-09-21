---
status: expected-red
kind: defect
opened: 2026-09-07
---

# `root_named_but_absent` missed the kernel's refusal inside its five-second window, once

`ci` run 34156449074, job 101849198160, `guest (9)`, on PR 437's head; every
other shard green in the same run, and 8 of 8 green on the dev host alone on
that branch.

`the kernel did not refuse this ROOT set`, and the capture it judged ends at
`gpt: device 16 carries the boot partition` — the refusal `rootfs::mount`
writes never arrived inside `boot_expecting_root_refusal`'s five seconds of
UART polling (`tests/common/volumes.rs`). The test is `Sched::Serial`, so
`ALONE: GREEN, and it was alone both times`: nothing the harness controls
differed and it failed once and passed once. Between the last line the capture
carried and the one it wanted is `smp::boot_aps`, which brings up every vCPU
and is the step on that path a loaded runner lengthens most.

Owed: the wait becomes a wait for the refusal line under a liveness ceiling
instead of a fixed five seconds, or the mechanism is shown to be something
else. `src/redlist.rs` quarantines the name until then.
