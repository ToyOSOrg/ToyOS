---
status: open
kind: defect
opened: 2026-10-07
---

# The shell runs a command whose redirect it could not open

`apply_redirect` (`userland/shell/src/main.rs`) answers a redirect target it
could not open by printing `cannot open: <path>` and returning, and its
callers spawn the command all the same, with the shell's own output in place
of the file. `echo secret > /somewhere/refused` therefore writes `secret` to
the terminal, or into whatever the shell's output is, and the line's status is
the command's. The `2>` arms do the same without the line.

Read from the source, not measured. `tests/toyos-rust-tests/src/bin/fs_share.rs`
launches a shell whose redirect's connection is refused and judges only that
the file was never made, so it does not hold this behaviour in place.

Owner: the shell.

## Exit condition

A command whose redirect could not be opened is not run, the shell says the
error the open answered, and the line's status is a failure; a guest probe
redirects into a path that does not open and reads back neither the command's
output nor a zero status.
