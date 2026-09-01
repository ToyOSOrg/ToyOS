---
status: open
kind: defect
opened: 2026-09-01
---

# `FAIRNESS_SAMPLE`'s table says "crossed by N ms" for a quantity that is not that

`scenarios::FAIRNESS_SAMPLE`'s doc table
(`toyos-sched/sim/src/scenarios.rs:723-734`, the crossed rows at `:728-731`) carries a verdict column reading
`**crossed**, by 116 ms in some window` at 4 CPUs and `by 324/418/634 ms` at
6, 8 and 12. The number in it is `Outcome::fair_over_bound`, which
`check_fairness` sets as `vm.fair_over_bound.max(spread)` for a window whose
spread exceeded the derived bound (`invariants.rs:719-720`) — the *widest
spread seen in a crossing window*, not the amount by which the bound was
crossed. The two are different quantities, and the table's own row shows it:
at 4 CPUs the worst spread is 198 ms against a 204 ms bound, so nothing was
crossed *by* 116 ms of anything.

`issues/kernel/granularity-bound-crossed-at-four-widths.md` records the same
correction for the tracker entry it was written in. The source doc was not
corrected with it, so the wrong-in-kind sentence still stands where a reader of
the simulator meets it.

The figure is also stale. Re-measured at `1ee9ec9a` on this host:

```
cargo run --release -p toyos-sched-sim -- measure fairness_storm:4 500
fairness_storm_smp: 500 runs, 1204198 steps, clean (I5 worst spread 198000000/204000000 ns, 112000000 ns PAST THE DERIVED BOUND on the recorded allowance, I5 reach 73%, I13 worst spread 28000000/60000000 ns, I13 reach 55%)
```

112 ms, against the table's 116.

Exit: re-run the whole ten-width sweep the table's provenance names and rewrite
the verdict column as what `fair_over_bound` is. The other nine widths were not
re-measured here, so the table may be stale in more places than the one figure
this checked.
