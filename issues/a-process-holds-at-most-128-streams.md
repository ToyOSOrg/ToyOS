---
status: open
kind: defect
opened: 2026-10-14
---

# A process holds at most 128 streams

The process's ends are a fixed table, `MAX_STREAMS` = 128 streams and 256
handles of their connections (`sdk/toyos/src/net/stream.rs`), since `toyos`
allocates nothing: past it a connect or an accept is refused
`ResourceExhausted` before anything is made. 128 is libc's `MAX_SOCKETS`, an
estimate no measurement set, and a new limit for a std program.

**Exit condition**: the table grown on demand, or its bound read off a
measured program's need.

**Owner**: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.
