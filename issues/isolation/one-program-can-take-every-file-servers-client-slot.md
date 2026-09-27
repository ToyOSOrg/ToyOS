---
status: open
kind: defect
opened: 2026-09-27
---

# One program can take every file server's client slot

`userland/fsd/src/main.rs`'s `MAX_SERVED` bounds the clients one file server
serves at once, machine-wide: the 129th hello is refused `ResourceExhausted`.
Any program holding `fs:/home` can open 128 connections and hold them, and
from then on every other program's first connection to DATA's directories is
refused, by name instead of by a hang. `test_rs_fs_client_bound` does exactly
this for the length of its run.

## Exit condition

A program's connections count against a bound of its own, so one program
holding all it may hold leaves another's first connection answered, shown by a
guest test that holds one program at its bound while another connects.
