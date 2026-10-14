---
status: open
kind: defect
opened: 2026-10-14
---

# A stream commits 8 MiB at its connect, and a process holds at most 128

Every connect and accept makes its region whole before a byte moves:
`toyos::net::stream::REGION_BYTES`, four 2 MiB pages from `SYS_SHM_CREATE`,
physically contiguous, where the pipes it replaced took a page on first write
and [tcp]'s rings grew as text was held. A program that opens many short
connections pays 8 MiB for each, and a fragmented machine can refuse a
connect a pipe would have taken. #863 accepted the cost with the design.

The process's ends are a fixed table, `MAX_STREAMS` = 128 streams and 256
handles of their connections (`sdk/toyos/src/net/stream.rs`), since `toyos`
allocates nothing: past it a connect or an accept is refused
`ResourceExhausted` before anything is made. 128 is libc's `MAX_SOCKETS`, an
estimate no measurement set, and a new limit for a std program.

**Exit condition**: regions whose pages are allocated as the rings fill, so a
stream commits what it holds; and the table grown on demand, or its bound
read off a measured program's need.

**Owner**: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.
