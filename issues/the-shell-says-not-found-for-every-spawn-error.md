---
status: open
kind: defect
opened: 2026-10-02
---

# The shell says "not found" for every spawn error

`execute_simple` and `execute_pipeline` (`userland/shell/src/main.rs`) match
the error of `Command::spawn` and `Command::status` as `Err(_)` and print
`<program>: not found`. A spawn refused for its depth, one under a place being
torn down and one refused `PermissionDenied` all read to the user as a program
that is not there.

Evidence: the T14's `proctreecase` boot at 1e9cb8c77. The kernel recorded
`spawn: refused under pid 79 at depth 65, more than 64 below init`, and the
shell at that depth printed `/system/bin/test_rs_process_tree: not found`.
`process_tree`'s chain (`tests/toyos-rust-tests/src/bin/process_tree.rs`)
accepts that line as its stop, and the row's judge reads the kernel's record
for the depth.

Exit: the shell prints the error the spawn answered, as its `detach` does, and
says `not found` for `ErrorKind::NotFound` alone; `process_tree`'s chain no
longer accepts `: not found` as its stop. Owner: the shell.
