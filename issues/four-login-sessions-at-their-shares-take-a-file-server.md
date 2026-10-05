---
status: open
kind: defect
opened: 2026-10-05
---

# Four login sessions at their shares take a file server

`userland/fileserver/src/main.rs` gives each session a grant names a quarter of
each machine-wide bound: 32 of `MAX_SERVED`'s 128 served connections, 8 of
`MAX_HANDSHAKES`' 32 waiting on their hello, 16 of `MAX_STREAMS`' 64 streams.
So four sessions at their shares take each bound whole, and a fifth session's
first connection or `STREAM` on that server is refused `ResourceExhausted`.

A session no longer gets more of a server by launching: every launch made in
it spends its share (`toyos_manifest::launch`). But sessions are cheap to
open. Every launch a `login` row makes opens one: each program the desktop's
taskbar starts (the compositor's row), and each program sshserver starts for a
client. A session reaches a `login` row of its own through `starts`: the
shell's and toybox's rows in `system.toml` list `sshserver`, and any process can
append a key to its list (`issues/sshserver-authorized-keys-unprotected.md`).

Nothing bounds how many sessions are open, so no fixed share answers every
session's first connection. Stage 4 of
`issues/every-program-sees-only-the-files-it-was-given.md` owns per-session
bounds.

## Exit condition

However many sessions are open, each holding all a server lets it hold, the
next session's first connection and first stream on that server are answered,
or opening it is refused by name; shown by a test that does exactly that.
