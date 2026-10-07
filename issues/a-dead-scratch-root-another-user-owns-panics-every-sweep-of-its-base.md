---
status: open
kind: defect
opened: 2026-10-07
---

# A dead scratch root another user owns panics every sweep of its base

`toyos-tmpdir`'s sweep moves every root whose process is gone into the
sweeping process's own root (`reap_into`, `toyos-tmpdir/src/lib.rs`) and
panics when the move is refused. In a sticky base such as `/tmp` a user who is
not root cannot rename a directory another user owns, so one dead root left by
another user's killed process panics the first `TempDir` of every later
process of every other user, until somebody removes it by hand. The module
header's claim that a directory the sweep cannot remove costs no process but
the one that met it holds of the removal (`stuck`) and not of the move before
it.

**Evidence:** on an Ubuntu 24.04 cloud container, a `cargo run -- --ci host`
run as root was killed mid-run and left `/tmp/toyos-tmp-23609-0`. The next
`cargo run -- --ci host`, run as a user that is not root, exited 101 before
its first step:

```
thread 'main' (928) panicked at toyos-tmpdir/src/lib.rs:321:33:
move /tmp/toyos-tmp-23609-0 to /tmp/toyos-tmp-928-0/reap-toyos-tmp-23609-0: Operation not permitted (os error 1)
```

Not reproduced since: it takes two users on one host, one of them killed
without unwinding.

**Owner**: `toyos-tmpdir`.

**Exit**: a process whose sweep meets a dead root it may not move still makes
its first directory and names that root once, and a test that hands the sweep
a move that is refused holds it.
