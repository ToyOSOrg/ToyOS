---
status: assigned
kind: defect
opened: 2026-09-27
---

# One program can take every file server's client slot

Held by the orchestrator.

`userland/fsd/src/main.rs`'s `MAX_SERVED` bounds the clients one file server
serves at once, machine-wide: the 129th hello is refused `ResourceExhausted`.
Any program holding `fs:/home` can open 128 connections and hold them, and
from then on every other program's first connection to DATA's directories is
refused, by name instead of by a hang. `test_rs_fs_client_bound` does exactly
this for the length of its run.

`MAX_STREAMS` is the same bound on streams: 64 a server, machine-wide, which
one program's clients can take whole, and every other program's `STREAM` — a
child's output into a served file — is then refused.

## Exit condition

A program's connections and streams each count against a share of their own,
so one program holding all it may hold leaves another's first connection and
first stream answered, shown by a guest test that holds one program at both
bounds while another connects and streams.
