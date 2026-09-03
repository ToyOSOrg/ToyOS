---
status: open
kind: tooling
opened: 2026-08-06
---

# Nothing connects to sshd, so its accept path is read-verified

`tests/sshdcase` boots sshd with a NIC and certifies the half that needs a
machine: that it mints an identity under `/home`, that it names the file it
authenticates against, and that with no usable key it exits instead of holding
port 22. The decision itself — this key yes, that key no, an options line
never — is host-tested in `userland/sshd`'s own `#[cfg(test)]` module against
real Ed25519 keys and `ssh-key`'s parser.

What neither reaches is a client. No test completes an SSH handshake, so the
wiring between russh's auth callbacks and that decision — `auth_publickey`,
`auth_publickey_offered`, and the `MethodSet` that stops password auth being
offered at all — is certified by reading. Closing it needs an SSH client on the
host talking to the guest through `hostfwd`, which belongs with the network gate
(`issues/build/there-is-no-network-gate.md`).

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling: an instrument
that is owed is what a defect is for, and this one guards a network-facing
authentication boundary). What is certified by reading is exactly the wiring
between russh's callbacks and the tested decision — `auth_publickey`,
`auth_publickey_offered` and the `MethodSet` that keeps password auth from being
offered — so a mis-wiring there passes every test in the tree. Owed with the
network gate (`issues/build/there-is-no-network-gate.md`), which is what puts a
client on the host.
