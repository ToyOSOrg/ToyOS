---
status: open
kind: defect
opened: 2026-09-30
---

# The shipped image carries three test programs

Root `CLAUDE.md`'s "Nothing ships for tests alone" is untrue of the shipped
`system.toml` in three rows:

- `proctest = {}`: `userland/proctest/README.md` calls it a test harness, and
  nothing in the tree runs `/system/bin/proctest` but `proctest` itself.
- `input-test = {}`: a test utility by its README, and nothing runs
  `/system/bin/input-test`; `toyos-symbols/tests/real.rs` reads a frozen copy
  under `toyos-symbols/tests/fixtures/`, not the shipped binary.
- `"bin/spin" = "/system/bin/toybox"`: `userland/toybox/src/spin.rs` loops
  forever, and every caller is a test: `tests/jobdeadlinecase`'s job list and
  `tests/common/ssh.rs` on `tests/sshdcase`.

Found by `git grep -n -w -e proctest -e input-test` and
`git grep -n -e 'bin/spin' -e '"spin"'`.

**Exit condition**: the shipped `system.toml` names none of the three, and a
test that needs one carries it in its own image alone.
