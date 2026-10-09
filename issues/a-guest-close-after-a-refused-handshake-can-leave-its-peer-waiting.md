---
status: open
kind: defect
opened: 2026-10-09
---

# A guest's close after a refused handshake can leave its peer waiting

`https_fetch` (`tests/toyos.rs`) runs `https_get` three times on one boot of
`tests/netcase`, each a new process and one TCP connection through
`/system/bin/netstack` (smoltcp, the stack `main` ships) to a `rustls` server
on the host. The second and third runs refuse the server's certificate during
the handshake, print their refusal, drop the connection and exit 2. On the
host, the server's read of the handshake's next record is answered by
nothing: in one boot its peer's end of stream arrived and in the other it did
not, 180 seconds after the process exited, no FIN and no reset. netstack
logged nothing about either connection.

The same client and server both on the host (macOS) see each refused
connection end at once with an end of stream and no alert: the client sends
no alert, so the close is all the peer is owed, and it is what is missing.

Measured at `a944746d5` plus the branch that added `https_fetch`, under QEMU
with slirp, once (logs on that branch's pull request): the
second connection of the boot never ended at the peer; the third did, by the
end of the 180 seconds. Two earlier boots of the same test reported the
second connection silent after 30 seconds. Not reduced: whether it is the
connection's state when its owner exits mid-handshake (the server's flight
unread, nothing sent since the client's first flight), or the second
connection of a boot, is unmeasured, and so is whether it reaches the stack
the move installs (`issues/toyos-has-its-own-network-stack.md`).
`OWNERLESS_LIFE` (`userland/netstack/src/main.rs`) should have reset it at 100
seconds and no reset arrived either.

**Exit condition**: a guest test whose process closes a connection mid-
handshake, with the peer's flight unread, and exits, sees the host's peer
read the end of the stream within a stated bound, on each of several
connections in one boot.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
