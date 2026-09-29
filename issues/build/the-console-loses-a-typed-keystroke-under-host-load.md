---
status: expected-red
kind: defect
opened: 2026-09-29
---

# The console loses a typed keystroke under host load

`screen_console_scroll` went red in the Fast run of PR #593 at `fa99614b`,
665 s in, on a heavily loaded host: `the console never echoed what was typed
at it: its input line reads "/home/toy> test_test_screen_churn 10" and does
not begin "/home/toy> test_rs_test_screen_churn 10". A keystroke was lost`
(`orch-runs/593r3-fast.log`, line ~462). It then passed 3 of 3 at the same
`fa99614b` and 3 of 3 at `main` `7e151819` (`orch-runs/scs-*.log`), 13–125 s
each — 6 of 6 green off the one red.

This is a real defect and not this file's neighbour's shape
(`issues/build/parallel-tests-red-under-other-suites.md`): the verdict is not
a wall-clock guard reporting its own workload as the cause, it is the guest's
own echo showing a character that never arrived. A keystroke the harness
typed never reached the input line.

Unknown: whether the loss is in QEMU's input delivery, the kernel's
i8042/USB path, the translator, or the console. It has flaked before —
`issues/build/parallel-tests-red-under-other-suites.md` recorded
`screen_console_scroll` retired on 2026-09-04 after 3 of 3 green beside a
full fast tier, which this sighting refutes.

**Exit condition.** Green in N of N loaded runs, with N and the load stated,
and the cause found.
