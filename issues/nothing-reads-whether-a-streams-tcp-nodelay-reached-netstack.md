---
status: open
kind: defect
opened: 2026-10-08
---

# Nothing reads whether a stream's TCP_NODELAY reached netstack

netstack answers `MsgType::TcpSetOption` done or with an error, and nothing
answers what a stream holds: the pipe ABI has no option read
(`toyos/src/net.rs`), and std's `nodelay()` and libc's `getsockopt` answer the
value their own side stored. So a setter that stores the value and sends
nothing is seen by no test. Measured on libc's `connect`, which hands over a
`TCP_NODELAY` set before the connection existed: with the hand-over deleted,
the guest case that sets the option before `connect` and reads it after
(`tests/netcase/nodelay_kept.c`, the `libc_sockets` test) stays green. The
only effect of the option is when a small write leaves the machine, which a
QEMU test may not time.

**Exit**: a test reads what the stack holds for a stream's Nagle switch, on
the node under a host test of its option call or through a guest's wire, and
turns red when libc's `connect` or std's `set_nodelay` stores the value and
sends no request.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`, with
the reset-stream option test of
`issues/a-stream-its-peer-reset-refuses-the-option-requests-a-host-answers.md`,
which asks the same request.
