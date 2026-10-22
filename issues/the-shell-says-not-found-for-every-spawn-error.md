---
status: open
kind: defect
opened: 2026-10-02
---

# The shell says "not found" for every spawn error

`execute_simple` and `execute_pipeline` (`system/shell/src/main.rs`) match
the error of `Command::spawn` and `Command::status` as `Err(_)` and print
`<program>: not found`. A spawn refused for its depth, one under a place being
torn down and one refused `PermissionDenied` all read to the user as a program
that is not there.

Evidence: the T14's `proctreecase` boot at 1e9cb8c77. The kernel recorded
`spawn: refused under pid 79 at depth 65, more than 64 below init`, and the
shell at that depth printed `/system/bin/test_rs_process_tree: not found`.
`process_tree` has since spawned its links directly and reads their own
refusal, so it no longer goes through the shell.

Exit: the shell prints the error the spawn answered, as its `detach` does, and
says `not found` for `ErrorKind::NotFound` alone. Owner: the shell.
