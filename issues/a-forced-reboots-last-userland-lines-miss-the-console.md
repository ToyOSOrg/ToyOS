---
status: open
kind: defect
opened: 2026-11-01
---

# A forced reboot's last userland lines can miss the serial console

`session_lifecycle` (`tests/common/sessionlifecycle.rs`) forces a waiting
reboot and then waits on the console for two userland lines written just
before it: the job's `session_lifecycle: ok, and the reboot is forced`, said
the statement before its terminate, and the supervisor's `power: the stop ends
what stays`. In some runs one of them never reaches the wire. The kernel's own
records still do: the forced stop's kills of the holder and its session, and
`Rebooting.`. So the stop did happen as the job asked, and only the lines are
lost. The harness then reports `STALLED: waiting for the job to reach the
forced reboot`, or `the supervisor never said what the forced reboot ended`.

**Measured** on the remote QEMU x86-64 runner, `cargo test --test
toyos-build -- session_lifecycle`, 2026-11-01:
- #1108 `18060b521` (byte-stream connections): 1 red of 4.
- #1121 `e396bb5f1` (whole-message connections): 3 red of 6 so far.

Faster connections shorten the time from the terminate to the stop, so the
window opens more often, but it is there without them.

Where the lines go between a program's log ring and the wire at a stop
(`logkeeper`'s flush answers the supervisor before the stop goes on;
`console: klogd let the wire go to the stop`) is not yet traced.

**Exit**: every userland line written before a forced stop's request reaches
the console before the machine resets, and `session_lifecycle` is green 20
runs in a row on the runner.
