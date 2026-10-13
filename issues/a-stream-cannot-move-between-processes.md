---
status: open
kind: defect
opened: 2026-10-12
---

# A stream cannot move between processes

Today a TCP stream's handle reads and writes the stream in any process that
holds it. The handle is the stream's two data pipes joined into one
connection (`make_socket_fd`, `sdk/std/sys/net/connection.rs`), which
`TcpStream` reads and writes with `syscall::read` and `write`.
`File::from_raw_fd` takes any handle (`sdk/std/os/io.rs`) and reads and writes
it with the same calls (`sdk/std/sys/fs.rs`), and `Stdio::from(File)` hands
it to a child, whose slot the kernel fills with a duplicate
(`sdk/std/sys/process.rs`; `build_child_handles`,
`kernel/src/loader/start.rs`). So an inetd-style child serves the stream on
its stdin and stdout for as long as the process that connected it holds its
`TcpStream`.

The session design takes that away
(`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`, stage 3; PR
#863, amendment E). After its answer, a stream's connection carries only
doorbells, and the region that holds the stream's bytes is mapped only by
the process that connected or accepted it. Read through a `File` or a
child's stdio, the handle would wait for ever or read doorbells as data, and
netstack would drain what the child writes as doorbells, with no error on
either side. Stage 3 makes std refuse both loudly instead.
`File::from_raw_fd` of a stream's handle panics with `toyos::net`'s refusal.
Every path that moves a handle into another process answers that refusal
before anything moves: a spawn's stdio slots, `CommandExt::inherit_handle`
into any slot, `CommandExt::endow` and `Command::provide`, each starting no
child, and `toyos::ipc`'s `send_handles`, which sends nothing. And
`toyos::net` refuses by name a handle the process holds no end for, std's
`TcpStream::from_raw_fd` included. A program that hands a stream to another
process by any of these, as an inetd-style server does through its child's
stdio, then fails loudly on ToyOS where today it works, and one that takes a
moved stream back as a `TcpStream`, as socket activation does and as no
ToyOS std builds today, is refused, though root `CLAUDE.md` says existing
Rust just works. The bare `dup`, `SYS_HANDLE_SEND` and `SYS_SPAWN` syscalls
are not checked.

Owner: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.

Exit: a stream moved by its handle reads and writes in the receiving process,
and the process that moved it holds no end.
