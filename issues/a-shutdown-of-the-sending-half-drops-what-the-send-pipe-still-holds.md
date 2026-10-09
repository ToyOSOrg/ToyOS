---
status: open
kind: defect
opened: 2026-10-08
---

# A shutdown of the sending half drops what the send pipe still holds

`handle_tcp_shutdown` (`userland/netstack/src/main.rs`) calls `socket.close()`
on the pass that reads the request. A client's bytes reach netstack through
its send pipe and its request through its connection, and nothing orders the
two: bytes the client wrote before it asked may still be in the pipe.
`bridge_piped` reads the pipe only while `send_room` holds, which is false
from `FIN-WAIT-1` on, so those bytes are never sent and the peer reads a
stream that ends short with a clean FIN.

Dropping the write end instead is the path that works: the bridge reads the
pipe to its end and closes the socket after the last byte.

Measured in a guest on `main`, twice, against the host kernel's TCP behind
QEMU's user network, with a peer that echoes what it read once the client's
FIN arrives: a client that wrote 1,048,576 bytes and shut its sending half
down at once read 65,536 bytes back as sent and then `ConnectionReset`, where
the same program on a host's TCP reads all 1,048,576 and then the end.
`netstack_socket_churn`
(`tests/toyos-rust-tests/src/bin/netstack_socket_churn.rs`) writes into the
pipe after the shutdown, which is the client's own error and not this.

**Exit condition**: a shutdown of the sending half closes the socket after
the bytes the pipe held when the request was read, and a guest test on
`tests/netcase` whose client writes, shuts down at once and reads the peer's
echo sees every byte it wrote.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
