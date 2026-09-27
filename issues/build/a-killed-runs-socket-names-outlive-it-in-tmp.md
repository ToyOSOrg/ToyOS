---
status: open
kind: tooling
opened: 2026-09-28
---

# A killed run's socket names outlive it in `/tmp`

`toyos_build::socketpath::short` names every harness Unix socket
`/tmp/toyos-<label>-<pid>-<n>.sock`, outside the run's `toyos_tmpdir` root,
because a lane path under a macOS `$TMPDIR` does not fit `sun_path`. A `Socket`
removes its name when dropped, on a return and on an unwind. A run killed
without unwinding (SIGKILL) leaves its QMP and tap names.
`toyos_tmpdir`'s sweep reclaimed them when they lived in the lane directory,
and nothing reclaims them now. `short` removes a stale name only when a later
process reuses its pid. CI's "nothing left in `$TMPDIR`" check cannot see
`/tmp`.

## Exit condition

A run killed by SIGKILL leaves no `/tmp/toyos-*.sock` name that the next run
does not remove, shown by a test that kills a run holding a socket and finds
the name gone after the next one starts.
