---
status: expected-red
kind: defect
opened: 2026-09-29
---

# The console loses typed keystrokes under host load

`screen_console_scroll` went red in the Fast run of PR #593 at `fa99614b`,
665 s in, on a heavily loaded host: `the console never echoed what was typed
at it: its input line reads "/home/toy> test_test_screen_churn 10" and does
not begin "/home/toy> test_rs_test_screen_churn 10". A keystroke was lost`.
The three characters "rs_" never reached the input line. It then passed 3 of
3 at the same `fa99614b` and 3 of 3 at `main` `7e151819`, 13–125 s each — 6
of 6 green off the one red.

No layer (QEMU input, i8042/USB, translator, console) has been measured yet.

**Exit condition.** The layer that drops the characters is identified and
fixed, and the test then passes 10 of 10 consecutive runs
under the host load of the original failure (other suites running in
parallel); then the redlist row goes.
