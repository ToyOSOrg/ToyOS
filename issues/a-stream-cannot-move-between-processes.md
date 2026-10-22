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
does, is refused, though root `CLAUDE.md` says existing Rust just works. What
replaces the refusal is a socket shared by its holders, not moved, as the
ruling below says.

**Ruled** (owner, 2026-10-21): sharing, inheritance and the surviving holders
are preserved: a socket given to a child stays the parent's too, and the
others keep working when one holder exits or is killed. Neither resetting
every holder's connection when one is interrupted nor a temporary refusal to
give a socket to a child is approved. Nginx's shared listeners establish its
inheritance and lifetime requirements. The ruling covers every socket kind,
so listeners and datagram sockets are shared as streams are.

Planning's reading of the evidence (2026-10-21): data a dead holder already
consumed need not be recovered.

The sharing and recovery design is engineering's and under review, outside
the tree. It proposes, and nobody has accepted, that the descriptor work's
spawn from the table (PR 4) lands after its stages for shared streams,
listeners and datagrams, and unconnected C sockets, since every interim rule
for an inheritable socket is rejected or unapproved.

Owner: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.

Exit: a stream, a listener and a datagram socket given to a child read, write
and accept in both processes at once, and each keeps working in the survivor
when the other holder exits or is killed.
