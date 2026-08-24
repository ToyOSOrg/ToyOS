---
status: open
kind: defect
opened: 2026-08-01
---

# The ring's closed flags are userland's to forge, and netd believes them

The kernel no longer reads `RingHeader::flags`: its own `readers`/`writers`
counts decided every one of the four sites that used to consult them, and the
flag — unlike the count — is in the page `SYS_PIPE_MAP` maps writable
(`toyos-abi/src/ring.rs`'s `close_reader`/`close_writer` are plain stores into
that page).

netd still reads them, at three sites in `userland/netd/src/main.rs`.
`bridge_piped` treats `rx_ring.is_reader_closed()` as "the client died" and
`tx_ring.is_writer_closed()` as "the client stopped writing, so close the
socket"; `cleanup_dead_listeners` aborts a listener's socket on the same bit.
Anyone who can map one of those pipes can set the bit and make netd tear the
connection down.

**Who that is has changed, and it is narrower than it was.** This file used to
reason from `may_open_pipe`, a relationship check whose own residual was that a
peer entitled to one of a creator's pipes was entitled to all of them. That
function is gone from the kernel. `sys_pipe_map` now asks
`handles.get_ref(h, Rights::MAP)`, so mapping a pipe's ring page requires
holding a handle to *that* pipe with `MAP` on it — the capability model, not a
relationship. The bound is therefore the handle graph, and for a piped
connection the process on the other end holds one by construction. So the
exposure is the connection's own client, and it is self-harm.

Self-harm is where it stops today, and it is not why this stays open. The
general statement, since it is the same one the kernel had to learn: **a
publication is not a channel.** netd is reading a value its peer writes and
treating it as a fact about its peer. The kernel's answer was to ask the side
that knows; netd has no such side to ask, which is the actual design gap. The
kernel-known facts that would replace the three reads are EOF on a read and
`BrokenPipe` on a write — see `issues/isolation/untrusted-sites-not-yet-adopted.md`.
