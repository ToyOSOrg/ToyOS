---
status: open
kind: defect
opened: 2026-10-08
---

# A connection libc accepts does not carry its listener's TCP_NODELAY

`setsockopt(IPPROTO_TCP, TCP_NODELAY)` on a stream socket that is not
connected is kept in libc's socket entry (`userland/libc/src/socket.rs`) and
handed to netstack by `connect`. A listener is never connected: its value is
kept, `getsockopt` answers it, and `accept` makes the new connection's entry
with the option clear and sends netstack nothing. POSIX does not say what an
accepted socket inherits. Measured on Darwin 27.0.0, one C program: the
option set on a listener before `bind` answers 0, set after `listen` answers
0, and `getsockopt` on the socket `accept` returns reads it set. Linux is
unread.

No guest test can ask: nothing dials into a guest. The harness's network is
QEMU's user backend with no forwarded port (`tests/common/qemu.rs`), and
whether a guest reaches its own address through netstack is unmeasured.

**Exit**: `accept` hands a listener's kept `TCP_NODELAY` to netstack for the
connection it answers, as `connect` does, and a guest C case accepts a
connection a host server's peer dials and reads the option set on it.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`: the
case needs a way into the guest, which is the harness's.
