---
status: open
kind: tooling
opened: 2026-11-04
---

# A budgeted guest can still share host CPUs

`toyos_build::buildlock::guest_cpus` gives a guest a host CPU for each of its
vCPUs, shared across every worktree's harness on the host. Two cases still
leave a guest's vCPUs descheduled, and the kernel's ticket locks then hand
over at the host's scheduling pace:

- **A guest wider than the host** takes every CPU and still runs
  oversubscribed by itself. The nightly `guest` runners have four cores, and
  the four `virtsmpcase` rows boot eight vCPUs, so the exit
  `a-counters-read-under-host-load-can-go-silent-for-15-s` asked for (a CPU
  for each vCPU) is not met there.
- **Load that is not a guest** takes nothing from the budget: cargo's jobs,
  other worktrees' builds, and a QEMU that `cargo run` starts. In the A/B that
  introduced the budget (2026-11-04: the four rows in two harness invocations
  at once, host load 19 to 53, 14 cores), every budgeted row passed, 32 of 32.
  One `test_rs_counters_read` nevertheless took 14.22 s of guest clock against
  `GUEST_QUIET`'s 15 s, with only its own guest admitted.

Exit: the rows that boot more vCPUs than their runner has run where it has as
many, and the build system's parallel jobs take their CPUs from the same
budget, or the four rows run beside a full build of another worktree, 24 of
24, with no read past half the bound.
