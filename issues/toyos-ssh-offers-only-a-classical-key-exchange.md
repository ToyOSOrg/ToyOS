---
status: open
kind: defect
opened: 2026-10-09
---

# toyos-ssh offers only a classical key exchange

`toyos-ssh` offers one key exchange, `curve25519-sha256` (`kex::METHODS`).
The server shipped today, on the russh fork, negotiates `mlkem768x25519-sha256`
with OpenSSH 9.9 and later: the pinned russh (`389804d`,
`russh/src/negotiation.rs:103`) offers it first. The switch would therefore
trade a post-quantum hybrid exchange for a classical one. Every session
recorded against `toyos-ssh` with OpenSSH_10.3p1 logs `** WARNING: connection
is not using a post-quantum key exchange algorithm.` (#817). `ring` 0.17.14 has
no ML-KEM.

The owner ruled "Write our own ML-KEM (Recommended)". The switch of
`userland/sshserver` waits for it, so no shipped server downgrades
(`issues/the-ssh-server-is-toyos-own-and-russh-is-gone.md`, stages 2 and 3).

**Owner**: `toyos-ssh`.

**Exit**:
- `toyos-ssh` offers `mlkem768x25519-sha256` first.
- NIST's ML-KEM-768 encapsulation known answers pass.
- An OpenSSH recording that negotiated it replays byte for byte, and its
  client's log has no post-quantum warning.
