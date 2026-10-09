---
status: open
kind: tooling
opened: 2026-10-09
---

# A job's capture holds every program's lines of its window, so a test that compares one whole is red under load only

`TestResult::stdout` is filed by `push_user_half` (`tests/common/qemu.rs`),
which takes every console line between a job's `===TEST_START===` and its
end marker that is not the kernel's. Whose line it is is not asked: a server
still finishing its boot when the job starts says its late lines into the
job's capture, and the head `logkeeper` gave each, which names its program,
is cut off by `user_text` before the test sees it. A test that compares a
capture whole is green alone, green in one suite and red in the next, by how
late the host's load makes a server.

Evidence: the suite at `6b1a9322f` exited 1 with 37 of 38, `uptime`'s
one-minute mean 29.6 as it began and 47.9 as it ended.
`nvme_disk_keeps_log_and_home` compared `cat` of a kept file with `cat` of
its source, byte for byte, as the first two jobs after a reboot's ready
marker, and said they differed. The kept file's read was its ten lines. The
source's was six of `acpiserver`'s boot lines and then the same ten, value
for value; as the round-2 review of pull request #797 read them off the
run's log, the six run from `table 1 of 1 (DSDT) loaded` to
`\_S5 handed to the kernel`:

```
acpiserver: table 1 of 1 (DSDT) loaded
… four more of acpiserver's …
acpiserver: \_S5 handed to the kernel…
NAME="…"
… nine more lines of the file, each `UPPER_CASE="…"` …
```

The four between and the head each line carried on the console were not
read again for this file. The same test passed alone at that head (exit 0)
and in the suite at `a4033a5d2` (exit 0, 38 of 38, load 56.8 to 45.1). It is
not reproduced on demand: no late line was staged.

`9a0660e79` holds both reads to the lines of the file's own form: the
`assignments` closure in `nvme_disk_keeps_log_and_home` (`tests/toyos.rs`)
keeps a line whose key before `=` is upper case and `_` only, which leans on
no program saying such a line and no longer sees a line of another form
added to the kept file. That test was the tree's only whole-capture
comparison when this was filed: `run_test`'s other callers look for one
line, a phrase, or an exit code.

**Exit condition.** `TestResult` hands a job its own lines, told from every
other program's by the head each line arrives with, and the `assignments`
filter is deleted from `nvme_disk_keeps_log_and_home`, which then compares
the two reads whole. The clause on `tests/CLAUDE.md`'s two-pieces bullet
that states this fact goes in the same change.

## Owner

`tests/common/qemu.rs`; unheld.
