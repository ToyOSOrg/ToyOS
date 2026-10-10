---
status: open
kind: defect
opened: 2026-10-09
---

# A guest's close after a refused handshake can leave its peer waiting

`https_fetch` (`tests/toyos.rs`) runs `https_get` three times on one boot of
`tests/netcase`, each a new process and one TCP connection through
`/system/bin/netstack` to a `rustls` server on the host. The second and third
runs refuse the server's certificate during the handshake, print their
refusal, drop the connection and exit 2. On smoltcp, the stack `netstack` ran
before ToyOS's own replaced it, the server's read of the handshake's next
record was answered by nothing: in one boot its peer's end of stream arrived
and in the other it did not, 180 seconds after the process exited, no FIN and
no reset. netstack logged nothing about either connection.

The same client and server both on the host (macOS) see each refused
connection end at once with an end of stream and no alert: the client sends
no alert, so the close is all the peer is owed, and it is what was missing.

Measured at `a944746d5` plus the branch that added `https_fetch`, under QEMU
with slirp, once (logs on that branch's pull request): the
second connection of the boot never ended at the peer; the third did, by the
end of the 180 seconds. Two earlier boots of the same test reported the
second connection silent after 30 seconds. Not reduced, and no capture was
taken of the guest's side, so slirp losing the guest's FIN was not ruled out.

On ToyOS's own stack, measured at that branch's merge of `main` after #801
by a probe patch on `https_fetch` that waits up to 150 seconds for each
refused connection's end (patch and logs on that branch's pull request): in
two boots, all four refused connections had already ended at the peer when
the harness looked, after the third run exited. Two boots are no rate, and
`https_fetch` still asserts nothing of these ends.

**Exit condition**: a guest test whose process closes a connection mid-
handshake, with the peer's flight unread, and exits, sees the host's peer
read the end of the stream within a stated bound, on each of several
connections in one boot.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`.
