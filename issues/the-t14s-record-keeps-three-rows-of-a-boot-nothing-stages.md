---
status: open
kind: tooling
opened: 2026-10-07
---

# The T14's record keeps three rows of a boot nothing stages

The T14's record, in the host's store under `metaltimings::dir()`, records
`boot.testcases-window.complete_ms`, `.panel_max_us` and `.panel_us`, and no
registration names a `testcases-window` boot: `grep testcases-window` on that
record finds those three lines, and `git grep testcases-window` finds nothing
but this issue. A whole run of the metal profile says so on every run and
stays green ("3 recorded number(s) this run measured nothing for"), so a row
whose boot is gone is never removed, and the same line would not red on a boot
that silently stopped being staged.

Owner: the metal suite (`tests/common/metal.rs`, `src/metaltimings.rs`).

**Exit:** the three rows are deleted from the T14's record under
`metaltimings::dir()`, and a whole run of the profile that
measures nothing for a recorded number is red by name; a filtered run, which
stages a part of the profile, still is not.
