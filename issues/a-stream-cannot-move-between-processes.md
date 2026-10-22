---
status: open
kind: defect
opened: 2026-10-12
---

# A stream cannot move between processes

A TCP stream is shared by its holders (`sdk/toyos/src/net/stream.rs`): the
process that connected or accepted it, and every process a spawn gave it to
as a descriptor, which exports a holder of it (`fd:<n>:stream`,
`sdk/toyos/src/fd.rs`) and which the child takes up. Each holder keeps
working when another exits or is killed. A stream's own handle still goes
nowhere: `endow` and `provide` of one are refused with `Misuse::Moved`.

Listeners and datagram sockets are not shared yet: their ids are ambient
(`issues/netstack-socket-ids-are-ambient.md`), a C socket that has not
connected is libc's entry alone, and the spawn from the descriptor table's
inheritable flag is not built. A spawn refuses a libc socket that is no
stream (`NotSupported`, an object of libc's own), and passes a std
listener's descriptor as its wake pipe's bare handle, which names nothing the
child can use.

**Ruled** (owner, 2026-10-21): sharing, inheritance and the surviving holders
are preserved: a socket given to a child stays the parent's too, and the
others keep working when one holder exits or is killed. Neither resetting
every holder's connection when one is interrupted nor a temporary refusal to
give a socket to a child is approved. Nginx's shared listeners establish its
inheritance and lifetime requirements. The ruling covers every socket kind,
so listeners and datagram sockets are shared as streams are.

Planning's reading of the evidence (2026-10-21): data a dead holder already
consumed need not be recovered.

The sharing and recovery design is accepted. Streams are shared; listeners
and datagram sockets come next as holder sessions, after the pipe page
(`issues/a-pipe-is-a-two-mebibyte-page-for-a-mapping-nothing-makes.md`), then
C sockets shared before they connect, and the descriptor work's spawn from
the table (PR 4) lands last, since every interim rule for an inheritable
socket is rejected or unapproved.

Owner: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.

Exit: a stream, a listener and a datagram socket given to a child read, write
and accept in both processes at once, and each keeps working in the survivor
when the other holder exits or is killed.
