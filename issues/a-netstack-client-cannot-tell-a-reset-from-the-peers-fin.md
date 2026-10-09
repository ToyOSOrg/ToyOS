---
status: open
kind: defect
opened: 2026-09-26
---

# A netstack client cannot tell a reset connection from the peer's FIN

A stream reaches its client as two pipes, and the receive pipe's end says only
that it ended. std (`ended`, `sdk/std/sys/net/connection.rs`) reads the kind
off the send pipe: one with no reader behind a receive pipe at its end is a
failure. libc reads no kind: its `recv` answers every end 0 and every refused
read `EIO` (`userland/libc/src/socket.rs`), so a C client reads a reset as the
peer's FIN.

On the node that order holds: a failure lets the send pipe go before the
receive pipe, and an orderly end keeps a send pipe whose last byte and FIN are
queued until its writer leaves (`userland/netstack/node/src/streams.rs`'s
header; its host tests in `userland/netstack/node/tests/streams.rs`, each red
with the order broken).

What `main` ships is netstack on smoltcp, whose `bridge_piped`
(`userland/netstack/src/main.rs`) closes the receive pipe and then, in the same
pass, the send pipe of a connection that is no longer open, whether it failed
or ended in order. A client that reads the end between the two reads a
failure as a FIN, and one that reads it after both reads an orderly end as a
failure. A client that shut its sending half down before the peer's FIN is
the second: its connection is done in both directions at that FIN. Measured
in a guest on `main`, std, against the host kernel's TCP behind QEMU's user
network, the same program on the harness host's TCP (macOS) as the oracle:

- a client that shut its sending half down with nothing pending, then read the
  peer's four bytes and its FIN, read the four and then `ConnectionReset`, in
  each of two runs, where the host read `Ok(0)`; a C client doing the same
  read `recv` 0 with `main`'s libc, which reads every end so, and
  `ECONNRESET` with a libc that reads the kind as std does;
- a client that read the peer's FIN and then shut its sending half down read
  its next read as `Ok(0)` in two runs and as `ConnectionReset` in two others,
  where the host read `Ok(0)`: the race between the two closes;
- a reset mid-stream, a reset after the client's half-close and a FIN before
  the client's write read as the host read them, in each of four runs.

**Exit condition**: netstack runs on the node, and on `tests/netcase`:

- a guest std test whose client shuts its sending half down reads its peer's
  bytes and then the end as an end, one whose peer resets a stream reads the
  reset as a reset, and each line it prints is the host kernel's for the same
  program against the same peer;
- libc reads an end's kind as std does, and a guest C case reads `recv` 0 at
  the peer's FIN after `shutdown(SHUT_WR)`, `ECONNRESET` on a reset mid-stream
  and on one after `SHUT_WR`, `send` `EPIPE` after `SHUT_WR` and `recv` 0
  after `SHUT_RD`; and it is red with `recv`'s probe of the send pipe replaced
  by a plain 0, with `shutdown` not marking the sending half shut, and with
  `recv` not answering 0 after `SHUT_RD`. The libc change and its host test
  were written for the node and are posted on pull request #803 for the move
  to carry with that case.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
