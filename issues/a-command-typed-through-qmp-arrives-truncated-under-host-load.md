---
status: open
kind: tooling
opened: 2026-10-24
---

# A command typed through QMP arrives truncated under host load

`session_lifecycle` types `detach /system/bin/hangupholder` into the
desktop's terminal, one QMP key press and release per character
(`tests/common/sessionlifecycle.rs`). Under heavy parallel host load the
guest can receive the command with keys missing, and the row then stalls
waiting for the holder's spawn.

Sightings:
- #1057, at `4bfe7e489`, in the 22-row QEMU filter with every image building
  cold (2471 s of build work across the workers). The shell received
  `detach /pholder`, and the supervisor's launcher said "no row declares
  /pholder". The row passed alone three times, and the same filter passed on
  its next run. `sessioncase.toml` declares no memory floors, and memory was
  not short. Logs: https://github.com/ToyOSOrg/evidence/tree/81614cc6bcad9dcd7bbcd47d8f09462d1da7c957/r8
  (`rows.log.gz`).
- #1060, at `04ec3f6f4`, with the host at 5.82×: `session_lifecycle` stalled
  once and was not reproduced in nine later runs. The cause was not found, and
  dropped keystrokes are a suspect.

Where the keys drop is not established. The candidates are QEMU's QMP key
injection and the i8042 model's queue, which the harness fills one keystroke
at a time with no wait for the guest to drain, or the guest's path from
ps2server through the compositor to the terminal.

Owner: the test harness. Exit: find where the keys drop, QMP or the guest's
input path, and fix it there. A guest that loses input is an input defect,
and pacing the harness to hide it is not an exit.
