---
status: open
kind: defect
opened: 2026-10-08
---

# A connection libc accepts does not carry its listener's TCP_NODELAY

A host's listener keeps `TCP_NODELAY` and the connections it accepts begin
with it. Measured on Darwin 27.0.0, one C program: the option set on a
listener before `bind` answers 0, set after `listen` answers 0, and
`getsockopt` on the socket `accept` returns reads it set. Linux is unread.

netstack has no option for a listener: `MsgType::TcpSetOption`
(`toyos/src/net.rs`) names a connection, and netstack answers a listener's id
`ERR_NOT_CONNECTED`. So libc (`userland/libc/src/socket.rs`):

- refuses `setsockopt(IPPROTO_TCP, TCP_NODELAY)` on a socket it has bound as
  a listener with -1 and `ENOTCONN`, keeps nothing for it, and `getsockopt`
  reads what the socket held before. `tests/netcase/nodelay_kept.c` holds the
  refusal and the read.
- keeps a value set before `bind`, as it keeps one set before `connect`, and
  the listener reads it back; `accept` makes the connection's entry with the
  option clear and sends netstack nothing. Read from the code, not run. That
  one is a program told yes and given no, and no guest test can read it:
  nothing dials into a guest, whose network is QEMU's user backend with no
  forwarded port (`tests/common/qemu.rs`).

Not carried by `accept` on smoltcp, by the orchestrator's decision: the stack
this track builds gives a connection the option its listener held when the
connection began (`Options`, `toyos-net-shard/tcp/src/lib.rs`, "inherited by a
listener's children", set by `Tcp::set_listener_options`), and a hand-over at
`accept` would carry one set after the connection was queued too, which is
another rule.

**Exit**: the stage of `issues/toyos-has-its-own-network-stack.md` that gives
the pipe ABI a listener's option: libc hands a listener's `TCP_NODELAY` to the
stack, at `bind` for one set before it and at the set for one after, both
answer 0, a connection `accept` returns reads the option its listener held
when it began, and `nodelay_kept.c`'s listener lines turn to the host's
answers. Until then the refusal stands.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
