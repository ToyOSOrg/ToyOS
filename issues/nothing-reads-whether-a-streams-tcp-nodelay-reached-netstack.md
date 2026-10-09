---
status: open
kind: defect
opened: 2026-10-08
---

# Nothing reads whether a stream's TCP_NODELAY reached netstack

netstack answers `MsgType::TcpSetOption` done or with an error, and nothing
answers what a stream holds once it is a client's: the pipe ABI says it once,
in the accept's answer (`TcpOptions`, `toyos/src/net.rs`), and has no option
read, and std's `nodelay()` and libc's `getsockopt` answer the value their own
side stored. So a setter of a stream's option that stores the value and sends
nothing is seen by no test. Measured on libc's `connect`, which hands over a
`TCP_NODELAY` set before the connection existed: with the hand-over deleted,
the guest case that sets the option before `connect` and reads it after
(`tests/netcase/nodelay_kept.c`, the `libc_sockets` test) stays green. The
only effect of the option is when a small write leaves the machine, which a
QEMU test may not time.

A listener's is read: what an accepted socket answers is what netstack said of
its connection, so libc's `bind` or its `setsockopt` on a listener storing the
value and sending nothing turns that case red.

**Exit**: a test reads what the stack holds for a stream's Nagle switch, on
the node under a host test of its option call or through a guest's wire, and
turns red when libc's `connect` or `setsockopt` on a connection, or std's
`set_nodelay`, stores the value and sends no request.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`, with
the reset-stream option test of
`issues/a-stream-its-peer-reset-refuses-the-option-requests-a-host-answers.md`,
which asks the same request.
