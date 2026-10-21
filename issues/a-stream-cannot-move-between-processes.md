---
status: open
kind: defect
opened: 2026-10-12
---

# A stream cannot move between processes

A TCP stream belongs to the process that connected or accepted it
(`sdk/toyos/src/net/stream.rs`). Its connection carries only the request, its
answer, doorbells and the hang-up; the stream's bytes are in a region mapped
only by that process. So std refuses every move loudly: `File::from_raw_fd` of
a stream's handle panics with `toyos::net::stream::Misuse::AsFile`
(`sdk/std/os/io.rs`); a spawn whose stdio, inherited or endowed descriptors
carry a stream's handle is refused with `Misuse::Moved` before any child
starts (`sdk/toyos/src/spawn.rs`, `sdk/toyos/src/fd.rs`'s `copy`,
`sdk/std/sys/pal/mod.rs`); and
`TcpStream::from_raw_fd` of a handle this process holds no end for panics with
`Misuse::NotThisProcess` (`sdk/std/os/io.rs`). The bare
`dup`, `SYS_MESSAGE_SEND` and `SYS_SPAWN` syscalls are not checked.

A program that hands a stream to another process, as an inetd-style server
does through its child's stdio, fails on ToyOS where it works elsewhere, and
one that takes a moved stream back as a `TcpStream`, as socket activation
does, is refused, though root `CLAUDE.md` says existing Rust just works.

Owner: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.

Exit: a stream moved by its handle reads and writes in the receiving process,
and the process that moved it holds no end.
