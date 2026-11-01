---
status: open
kind: defect
opened: 2026-10-12
---

# A socket libc or socket2 has not bound is not shared with a child

A TCP stream, a listener and a datagram socket are shared by their holders
(`sdk/toyos/src/net/stream.rs`, `sdk/toyos/src/net/socket.rs`): the process
that connected, accepted, listened or bound, and every process a spawn gave a
descriptor of it, which exports a holder (`fd:<n>:exported`,
`sdk/toyos/src/fd.rs`) that the child takes up. A stream's holder keeps
working when another exits or is killed. A stream's own handle stays in its
process: `endow` and `provide` of one are refused with `Misuse::Moved`.

`netstack_holders_exported_to_children` (`tests/toyos.rs`) reads a listener's
and a datagram socket's holders in a parent and its child, one after the other
and both waiting on one queue; no guest test reads a survivor after the other holder
is killed.

What is not shared:
- A C socket that has neither bound nor connected is libc's entry alone, an
  object of libc's own, which a spawn refuses (`NotSupported`).
- A socket2 socket's descriptor is an object of socket2's own, so a spawn
  refuses it (`NotSupported`), and `Socket::from_raw_fd` of a stream's
  descriptor answers every operation "invalid socket fd".
- The spawn from the descriptor table's inheritable flag is not built: a child
  gets its stdio and what its spawn names, never every inheritable descriptor.

Each child descriptor naming a stream is a holder of its own: inetd's
`dup2(s, 0); dup2(s, 1); dup2(s, 2)` makes three holders for one child, each
a place, a watch and four pipes. One
holder shared by a child's duplicates would need the child to alias an
adoption, since a second copy of one exported end carries no `ADOPT`.

**Ruled** (owner, 2026-10-21): sharing, inheritance and the surviving holders
are preserved: a socket given to a child stays the parent's too, and the
others keep working when one holder exits or is killed. Neither resetting
every holder's connection when one is interrupted nor a temporary refusal to
give a socket to a child is approved. Nginx's shared listeners establish its
inheritance and lifetime requirements. The ruling covers every socket kind,
so listeners and datagram sockets are shared as streams are.

Planning's reading of the evidence (2026-10-21): data a dead holder already
consumed need not be recovered.

Order: C sockets shared before they bind or connect; the spawn from the
inheritable flag last, since every interim rule for an inheritable socket is
rejected or unapproved.

Owner: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.

Exit: a stream, a listener and a datagram socket given to a child read, write
and accept in both processes at once, and each keeps working in the survivor
when the other holder exits or is killed.
