---
status: none
kind: rejected
opened: 2026-11-11
---

# The guest profile keeps its checks, which cost under five percent

**Declined by the orchestrator on 2026-11-11**, in the wakeperf assignment's
brief: the kernel and userland keep `[profile.toyos]`'s debug assertions and
overflow checks.

**What was measured.** One boot a bench of the osbench `system`, `sched`,
`ipc` and `time` groups, on main 422915e2c's shipping kernel and on the same
tree with `debug-assertions` and `overflow-checks` off for the kernel and
userland alike. Across the 47 rows the median ran 2.8% faster without the
checks on the T14 and 4.8% on the AMD bench; the quartiles were 1.00 and
1.07 on the T14 and 1.00 and 1.09 on the AMD bench. One boot a side settles
no single row, and the rows were read as an effect size only.

**Unexplained, one boot each.** Without the checks the T14 read `cpu.chunk`
at 13,757 ns against 3,000 and `mem.map_unmap_4k` at 2,793 ns against 1,929,
and the AMD bench read `ipc.reqrep.*` 1.2 to 1.9 times slower. Removing
checks removes work, so these are taken as noise or code layout unless a
repeated reading shows them again.

Evidence: the `unasserted` column of the `tables/*-compare.md` files in
https://github.com/ToyOSOrg/evidence/tree/3ddc686338d20b13528b597a7437e3b027479728/diagnosis.
