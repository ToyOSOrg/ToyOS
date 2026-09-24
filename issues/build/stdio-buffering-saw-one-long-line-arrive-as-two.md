---
status: open
kind: defect
opened: 2026-09-22
---

# `90_stdio_buffering` saw one long line arrive as two, beside other guests

A whole fast tier on this host (TCG, one suite), on branch
`wt/toyos-usbtransport` at `a0926caa`:

```
FAIL c::90_stdio_buffering: output mismatch
```

The expected capture has the program's run of `x` as one 10000-byte line. The
capture held it as two, 7168 and 2832 bytes (7168 is 7 KiB), with the lines
before and after it (`after flush`, `stderr line`) in order and intact.
The harness's own run alone was green (`ALONE 90_stdio_buffering: GREEN — it
fails only beside other guests`), and a second whole fast tier on the same
head minutes later was green (345 passed).

Not measured: whether the base reds the same way, or at what rate. The split
is either the guest writing the line in two console writes with something
between them, or the host's capture cutting it (`tests/common/console.rs`
unjoins a line without a trailing newline from the next writer's). Which one
is the question; `console_line_atomicity` is the gate that should hold the
first.
