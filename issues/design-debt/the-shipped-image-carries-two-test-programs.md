---
status: open
kind: defect
opened: 2026-09-30
---

# The shipped image carries two test programs

"Nothing ships for tests alone" (`.claude/agents/reviewer.md`, Fit) is untrue
of the shipped `system.toml` in two rows:

- `proctest = {}`: `userland/proctest/README.md` calls it a test harness, and
  nothing in the tree runs `/system/bin/proctest` but `proctest` itself.
- `"bin/spin" = "/system/bin/toybox"`: `userland/toybox/src/spin.rs` loops
  forever, and nothing in the tree runs it; its one named use is a test, the
  metal `sshd_exec` row
  `issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` owes.

Found by `git grep -n -w -e proctest` and
`git grep -n -w -e spin -- system.toml tests userland issues/build`.

**Exit condition**: the shipped `system.toml` names neither, and a test that
needs one carries it in its own image alone.
