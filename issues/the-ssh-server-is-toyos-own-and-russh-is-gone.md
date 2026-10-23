---
status: open
kind: track
opened: 2026-10-09
---

# The SSH server is ToyOS's own, and russh is gone

`system/sshserver` serves SSH on `toyos-ssh` (`lib/ssh`), ToyOS's own sans-IO
server core on `toyos-crypto` (`lib/crypto`), under a `std` driver. What is
left is retiring the mio and socket2 forks and a metal row.

**Owner**: `toyos-ssh` and `system/sshserver`.

The owner's rulings:

- **Post-quantum key exchange**: "Write our own ML-KEM (Recommended)". The
  server on russh negotiated `mlkem768x25519-sha256` with OpenSSH 9.9 and
  later, so stage 3 landed before stage 2, and the switch kept it.
- **Our own crypto** (2026-10-22): "Use our own crypto where possible; we
  optimize and improve until done, but we can already use it." The switch
  did not wait for stage 4 of
  `issues/toyos-crypto-is-in-the-tree-on-par-with-ring-and-secure.md`.
- **The tokio, mio and socket2 forks, after the switch**: "Retire them".
- **The oracle**: "Recorded sessions only (Recommended)". OpenSSH's `ssh` is a
  development instrument that records sessions for replay. No test runs it, and
  the guest suite runs no host `ssh` or `sftp`.

Compromises recorded on their own:
`issues/toyos-ssh-leaves-its-secrets-in-freed-memory.md`,
`issues/sshserver-cannot-end-a-program-that-closed-its-output.md` and
`issues/sshserver-reports-a-killed-program-as-exit-lost.md`.

## Stages

1. **The core, on the host.** `toyos-ssh`: strict `curve25519-sha256`, the
   `ssh-ed25519` host key from sshserver's existing `openssh-key-v1` file,
   `chacha20-poly1305@openssh.com`, `publickey` authentication with Ed25519
   keys, and `session` channels serving `exec`. Its tests are OpenSSH_10.3p1
   recordings replayed byte for byte, RFC vectors and a structured fuzz, under
   `cargo run -- --ci host`. Landed by #817. Nothing but its tests calls
   `Server` or the connection layer until stage 2.
2. **The switch and the deletion.** `system/sshserver` is rewritten on
   `toyos-ssh` with `std` threads: a shell over pipes, `subsystem sftp` over
   `sftp.rs`, `exec` as `shell -c` (#1024), an `authorized_keys` reader in
   `toyos-ssh`, several channels on one connection, a login deadline and a cap
   on unauthenticated connections. The recorded shell, sftp and two-channel
   sessions replay against `toyos-ssh`'s scripted driver, whose SFTP server is
   in memory so that its replies are the same on every host. `sftp.rs` is held
   by its own tests. russh, tokio, `rsa` and the `p384_backend` setting are
   gone. Landed by #1026.
3. **ML-KEM.** Our own `mlkem768x25519-sha256` (RFC 10042), on
   `toyos-crypto`'s ML-KEM-768 and its hybrid: the server's encapsulation
   side, offered before `curve25519-sha256`. Every OpenSSH recording but one
   negotiated it; the one forced onto `curve25519-sha256` keeps the classical
   method's replay. Landed by #1023.
4. **The forks retire.** tokio left with stage 2. mio and socket2 remain only
   as host dev-dependencies of `system/netstack/node`.
   **Exit**: the root `[patch.crates-io]` names neither `ToyOSOrg/mio` nor
   `ToyOSOrg/socket2`.
5. **On the T14.** A metal row logs in to sshserver, runs an `exec`, and moves
   100 MB of standard input whose hash is checked. Which client the row drives
   is not decided.
   **Exit**: the row is in the metal set and green.
