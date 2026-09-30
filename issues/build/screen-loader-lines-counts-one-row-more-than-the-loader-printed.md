---
status: open
kind: tooling
opened: 2026-09-30
---

# `screen_loader_lines` counts one row more than the loader printed

Red on three of the five nightlies from 2026-09-28 to 2026-09-30, each time
with the same sentence:

```
FAIL screen_loader_lines: the panel carried 26 rows at the GOP query and 41 at the loader's last line, a growth of 15, where the loader printed 14 lines between them, 14 rows at 240 columns
```

- red: `main` at `7e151819e` (run 36550208853, `guest (11)`), #625 at
  `2d45623ae` (run 36600425263, `guest (4)`), `main` at `ace064f9d` (run
  36696295750, `guest (8)`);
- green: `main` at `a7cd32765` (run 36400924827, `guest (4)`) and
  `wt/toyos-reap` at `6d82ff9fa` (run 36496779560, `guest (6)`).

Both green heads are ancestors of the first red one, and between them `main`
took five loader commits: `1090cf6ea`, `26ad88cfe` (the root bridges'
descriptor-list dump goes, which the test's own row count named as the line
that wraps), `6d422562f`, `561349db2` and `8565cc597`. Not bisected, so a
regression among them and a race the test has always had are both open. The
dev host's earlier sightings in
`issues/build/parallel-tests-red-under-other-suites.md` read differently: no
bands at all, and a growth of 0.

What the test does: two boots, each ended at a loader line, each panel dumped
after that line reached the console while the loader goes on drawing, so the
count is of whatever the panel held when the dump landed.

**The test is deleted**: `51e64b394` took it out, and `git revert 51e64b394`
brings it back.

**Exit**: the cause of the extra row named from a bisect or a dump taken where
the loader holds still, the test restored and green on the nightly, and
green beside other guests on the dev host.
