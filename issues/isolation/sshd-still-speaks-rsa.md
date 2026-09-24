---
status: open
kind: defect
opened: 2026-09-24
---

# sshd still carries RSA

`userland/sshd/Cargo.toml` enables russh's `rsa` feature, which locks the `rsa`
crate at 0.9.10. That crate has an unfixed timing side channel (the Marvin
attack, RUSTSEC-2023-0071) in its private-key operations. sshd's host key is
Ed25519, so those operations never run here, but RSA is code with no use:
only client keys could still be RSA, and a modern client offers Ed25519.

**Exit**: the feature is off, `rsa` is gone from `userland/Cargo.lock`, and a
test shows an RSA client key refused by name.
