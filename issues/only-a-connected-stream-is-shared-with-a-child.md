---
status: open
kind: defect
opened: 2026-10-12
---

# Only a connected stream is shared with a child

A TCP stream is shared by its holders (`sdk/toyos/src/net/stream.rs`): the
process that connected or accepted it, and every process a spawn gave a
descriptor of it, which exports a holder (`fd:<n>:stream`,
`sdk/toyos/src/fd.rs`) that the child takes up. Each holder keeps working when
another exits or is killed. A stream's own handle stays in its process:
`endow` and `provide` of one are refused with `Misuse::Moved`.

Every other socket is not shared:
- Listeners and datagram sockets are named by ambient ids
  (`issues/netstack-socket-ids-are-ambient.md`). A spawn passes a std
  listener's descriptor as its wake pipe's bare handle, which names nothing the
  child can use.
- A C socket that has not connected is libc's entry alone, an object of
  libc's own, which a spawn refuses (`NotSupported`).
- A socket2 socket's descriptor is an object of socket2's own, so a spawn
  refuses it (`NotSupported`), and `Socket::from_raw_fd` of a stream's
  descriptor answers every operation "invalid socket fd".
- The spawn from the descriptor table's inheritable flag is not built: a child
  gets its stdio and what its spawn names, never every inheritable descriptor.

Each child descriptor naming a stream is a holder of its own: inetd's
`dup2(s, 0); dup2(s, 1); dup2(s, 2)` makes three holders for one child, each
a place, a watch and four pipe pages
(`issues/a-pipe-is-a-two-mebibyte-page-for-a-mapping-nothing-makes.md`). One
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

Order: listeners and datagram sockets as holder sessions, after the pipe page;
then C sockets shared before they connect; the spawn from the inheritable flag
last, since every interim rule for an inheritable socket is rejected or
unapproved.

Owner: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`.

Exit: a stream, a listener and a datagram socket given to a child read, write
and accept in both processes at once, and each keeps working in the survivor
when the other holder exits or is killed.
