---
status: open
kind: tooling
opened: 2026-11-04
---

# The guest CPU budget counts guests and not builds

`toyos_build::buildlock::guest_cpus` gives every guest a host CPU for each of
its vCPUs, shared across every worktree's harness on the host. Nothing else
that loads the host takes from it: cargo's jobs, other worktrees' builds, and a
QEMU that `cargo run` starts. A guest admitted on a host busy with builds
still has its vCPUs descheduled. The kernel's ticket locks then hand over at
the host's scheduling pace.

**Measured.** In the A/B that introduced the budget (2026-11-04, the four
`virtsmpcase` rows in two harness invocations at once, host load 19 to 53, 14
cores), every budgeted row passed, 32 of 32. One `test_rs_counters_read`
nevertheless took 14.22 s of guest clock against `GUEST_QUIET`'s 15 s, with
only its own guest admitted. Main without the budget went silent 5 of 32.

Exit: the build system's parallel jobs take their CPUs from the same budget,
or the four rows run beside a full build of another worktree, 24 of 24, with
no read past half the bound.
