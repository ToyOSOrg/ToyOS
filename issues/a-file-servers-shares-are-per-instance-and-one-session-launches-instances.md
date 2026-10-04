---
status: assigned
kind: defect
opened: 2026-10-04
---

# A file server's shares are per instance, and one session launches instances

Held by the orchestrator.

`userland/fileserver/src/main.rs` gives each instance a grant names a quarter
of each machine-wide bound: 32 of `MAX_SERVED`'s 128 served connections, 8 of
`MAX_HANDSHAKES`' 32 waiting on their hello, 16 of `MAX_STREAMS`' 64 streams.
An instance is one start and the children it spawns directly, and the
supervisor mints a fresh one on every start. So four instances at their shares
take each bound whole, and every other program's first connection or `STREAM`
on that server is refused `ResourceExhausted`.

Four instances are cheap to have: a program whose row lists `starts` (the
compositor, terminal, shell, toybox and sshserver rows of `system.toml`) gets
a new instance on every launch, and a shell launches shells. One session can
therefore take DATA's server from every other session.

The shares are sized against what was measured, not against a session's need.
With the shares in place, every boot of the guest suite, the shipping
desktop booted under QEMU, and every shipping Rust job run one after another in
one `tests/testcases` boot never had an instance past 3 served connections, 1
waiting on its hello or 0 streams, nor more than 5 of DATA's connections served
at once; the desktop at rest holds 3. A parallel build (cargo with rustc as its
direct children) is the obvious unmeasured load.

## Exit condition

A server's bounds are shared per session, so that one session holding all its
programs may hold — through as many launches as its `starts` allow — leaves
another session's first connection and first stream on that server answered,
shown by a test that does exactly that.
