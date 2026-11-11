---
status: open
kind: finding
opened: 2026-11-11
---

# All-CPU throughput in one group reads under Linux's

`osbench fair`'s `fair.shared` runs a build-shaped job, its helpers and a
threaded job together in one group on every CPU, and `fair.shared.total` is
the compute chunks all of them finished a second
(`tests/guest/osbench/src/fair.rs`).

From the 2026-11-11 scoreboard ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65,
four boots a bench off its NVMe, interleaved with five runs of the bench's
Linux as installed. A cell is the median of the runs' p50s with their
run-to-run range. The AMD bench's Linux timed runs 1 to 3 through the HPET
and runs 4 and 5 through the TSC.

| row | T14 ToyOS | T14 Linux | AMD ToyOS | AMD Linux, HPET runs | AMD Linux, TSC runs |
|---|---|---|---|---|---|
| `fair.shared.total` | 2,369,620 chunks/s (0.1%) | 2,391,673 chunks/s (0.0%) | 4,062,194 chunks/s (0.8%) | 3,314,081, 3,301,534, 3,306,801 | 5,089,421, 5,099,351 |

On the T14 ToyOS reads 0.9% under Linux, resolved: 2.367 to 2.370 M against
2.3915 to 2.3918 M. On the AMD bench ToyOS's four boots read 4.05 to 4.08 M,
a fifth under Linux's two TSC runs and above its three HPET runs; the row is
unresolved there, and two runs are exploratory. The same boots'
`fair.apart.total`, each workload in a group of its own, reads 5.06 M on the
AMD bench, level with Linux's TSC runs, and 2.36 M on the T14. The
2026-10-28 scoreboard read the same shape on both benches. Why the one-group
total is lower than the apart total on the AMD bench is unmeasured.

**Exit**: `fair.shared.total` at Linux's on both benches with at least four
TSC runs of the AMD bench's Linux, or the difference measured and justified.

**Owner**: the scheduler (`kernel/src/sched/`).
