---
status: open
kind: defect
opened: 2026-10-08
---

# netstack keeps the socket of a connection whose client sent no close

A piped TCP stream has three things in netstack
(`userland/netstack/src/main.rs`): the bridge in `piped_connections`, the
smoltcp socket in the socket set with its two 64 KiB buffers, and the id's
entry in `sockets`. A client's close request removes all three. When a
connection ends any other way — the peer finishes or resets and the client
drops its pipe ends, or the client dies — `bridge_piped` removes the bridge
and nothing else. The socket and its entry stay for the life of the process.

`max_piped_connections` counts bridges and pending connects, so each ended
connection gives its slot back while its socket stays: the bound netstack
states for what clients can make it hold does not cover this, and every
client holding the `netstack` connector can grow it one connection at a time.
`inspect` reports the entries in `net.sockets.tcp` and has no count that tells
a kept one from a live one.

**Measured** at `06bbc236d`, on `tests/netcase` under QEMU against a host
server that ends each connection at once:
`cargo test --test toyos-build -- netstack_socket_churn` exits 1 on

    netstack_socket_churn: 4 connections ended and let go; netstack held 0 stream(s) before them and holds 4 after

each of the four read after `net.piped.live` had returned to its reading
before the first. The same run with the close request sent before the drop
exits 0.

**Exit condition**: a connection netstack lets go leaves no socket and no
table entry, whoever ended it, and `netstack_socket_churn` is green.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.

**Its test is deleted**: `8dd55d06a` took `netstack_socket_churn` out, with
the `netstack` connector `tests/netcase`'s runner held for it, and
`git revert 8dd55d06a` brings both back.
