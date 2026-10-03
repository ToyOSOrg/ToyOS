---
status: open
kind: defect
opened: 2026-09-30
---

# The shipped image carries three test programs

"Nothing ships for tests alone" (`.claude/agents/reviewer.md`, Fit) is untrue
of the shipped `system.toml` in three rows:

- `proctest = {}`: `userland/proctest/README.md` calls it a test harness, and
  nothing in the tree runs `/system/bin/proctest` but `proctest` itself.
- `input-test = {}`: a test utility by its README, and nothing runs
  `/system/bin/input-test`; `toyos-symbols/tests/real.rs` reads a frozen copy
  under `toyos-symbols/tests/fixtures/`, not the shipped binary.
- `"bin/spin" = "/system/bin/toybox"`: `userland/toybox/src/spin.rs` loops
  forever, and nothing in the tree runs it; its one named use is a test, the
  metal `sshd_exec` row
  `issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` owes.

Found by `git grep -n -w -e proctest -e input-test` and
`git grep -n -w -e spin -- system.toml tests userland issues/build`.

**Exit condition**: the shipped `system.toml` names none of the three, and a
test that needs one carries it in its own image alone.
