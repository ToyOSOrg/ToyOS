---
status: open
kind: defect
opened: 2026-10-09
---

# toyos-ssh leaves its secrets in freed memory

`toyos-ssh` frees every secret it holds without overwriting it.

- `transport.rs`, `on_ecdh_init`: the shared secret `K` sits in a `Vec` (as
  an `mpint`).
- `kex::derive` returns each 64-byte key on the stack.
- The host key's seed passes through the buffers `HostKey::from_openssh`
  decodes the file into.

A later read of freed heap or stack, by a bug in this process or by anything
that can read its memory, finds the session's keys. What `ring` keeps of these
inside `SealingKey`, `OpeningKey`, `EphemeralPrivateKey` and `Ed25519KeyPair`,
and whether it wipes it, has not been read.

**Owner**: `toyos-ssh`.

**Exit**:
- Every buffer `toyos-ssh` fills with `K`, a derived key or the host key's
  seed is overwritten before it is freed.
- What `ring` keeps of them has been read, and is stated at the site.
