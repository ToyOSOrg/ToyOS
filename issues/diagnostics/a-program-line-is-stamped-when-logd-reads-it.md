---
status: open
kind: defect
opened: 2026-09-25
---

# A program's line is stamped when logd reads it, not when it was written

`logd` stamps each line of a program's output with the monotonic time it read
the line out of the program's pipe (`userland/logd/src/main.rs`'s
`read_origins`), and orders `/log` by those stamps among the kernel's records.
A pipe carries no time, so a line written before a kernel record and read
after it sorts after that record. The common case: a program's last words — a
panic message — land in `/log` after the kernel's `exit:` record for the same
program.

What does hold: a line is never put ahead of a record written before it was
read (`log_program_line_after_its_records`).

## Exit condition

A program's line carries the time it was written — the pipe records a write's
time with its bytes, or a line's writer stamps it through a channel `logd`
can trust — and a test whose program writes a line and then exits finds the
line before its `exit:` record in `/log`.
