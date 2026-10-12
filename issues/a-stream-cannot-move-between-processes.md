---
status: open
kind: defect
opened: 2026-10-12
---

# A stream cannot move between processes

A program that hands a TCP stream to another process, as socket activation or
an inetd-style child does, cannot do so on ToyOS, though root `CLAUDE.md` says
existing Rust just works. A process holds a stream only by connecting or
accepting it. At ab655a115 nothing in the tree takes a stream handle another
process made: std on ToyOS has no socket `FromRawFd` or `From<OwnedFd>` (the
`rust/` fork's `library/std/src/os/fd/mod.rs` compiles `net` out for `toyos`),
libc's and socket2's sockets are keys in their own process's tables
(`SOCKET_FD_BASE`, `sdk/libc/src/socket.rs`; socket2's `NEXT_FD`), and the mio
fork's ToyOS stream has no `from_std`.

The session design keeps it so
(`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`, stage 3; PR
#863, amendment E): after its answer a stream's connection carries only
doorbells, so a process handed the handle has no channel on which to ask
netstack for the stream's region, and `toyos::net` refuses by name a handle the
process holds no end for, std's `FromRawFd` included once stage 3 adds it.

Owner: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.

Exit: a stream moved by its handle reads and writes in the receiving process,
and the process that moved it holds no end.
