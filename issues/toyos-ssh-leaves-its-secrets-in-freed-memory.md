---
status: open
kind: defect
opened: 2026-10-09
---

# toyos-ssh leaves its secrets in freed memory

`toyos-ssh` frees every secret it holds without overwriting it.

- `transport.rs`, `on_kex_init`: the shared secret `K` sits in a `Vec` (an
  `mpint` for `curve25519-sha256`, a `string` for `mlkem768x25519-sha256`).
- `kex::derive` returns each 64-byte key on the stack.
- The host key's seed passes through the buffers `HostKey::from_openssh`
  decodes the file into.

A later read of freed heap or stack, by a bug in this process or by anything
that can read its memory, finds the session's keys. `toyos-crypto`'s
`ssh::SealingKey` and `ssh::OpeningKey`, `x25519::SecretKey` and
`SharedSecret`, `mlkem::SharedSecret`, `hybrid::SshSecret` and
`ed25519::SigningKey` overwrite what each owns when it
drops (`lib/crypto/src/secret.rs`). The keys and the seed `toyos-ssh` hands
them by value, and any copy the compiler makes, are not overwritten.

**Owner**: `toyos-ssh`.

**Exit**:
- Every buffer `toyos-ssh` fills with `K`, a derived key or the host key's
  seed is overwritten before it is freed.
- What `toyos-crypto` keeps of them is stated at the site.
