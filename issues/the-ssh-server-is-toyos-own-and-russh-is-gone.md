---
status: open
kind: track
opened: 2026-10-09
---

# The SSH server is ToyOS's own, and russh is gone

`userland/sshserver` serves SSH on the `russh` fork over `tokio`. The root
`[patch.crates-io]` carries `russh`, `russh-cryptovec` and `tokio` for it, and
`Cargo.lock` carries the closure they bring: `rsa` (RUSTSEC-2023-0071, the
Marvin attack), `pageant` and `internal-russh-num-bigint` among it. This track
puts the server on `toyos-ssh`, ToyOS's own sans-IO server core on `ring`,
under a `std` driver, and deletes the russh closure in the landing that
switches.

**Owner**: `toyos-ssh` and `userland/sshserver`.

The owner's rulings:

- **Post-quantum key exchange**: "Write our own ML-KEM (Recommended)". The
  server shipped today negotiates `mlkem768x25519-sha256` with OpenSSH 9.9 and
  later, because the pinned russh (`389804d`, `russh/src/negotiation.rs:103`)
  offers it first. So stage 3 lands before stage 2, and the switch never
  downgrades the exchange.
- **The tokio, mio and socket2 forks, after the switch**: "Retire them".
- **The oracle**: "Recorded sessions only (Recommended)". OpenSSH's `ssh` is a
  development instrument that records sessions for replay. No test runs it, and
  the guest suite runs no host `ssh` or `sftp`.

Compromises of `toyos-ssh` recorded on their own:
`issues/toyos-ssh-offers-only-a-classical-key-exchange.md`,
`issues/toyos-ssh-leaves-its-secrets-in-freed-memory.md` and
`issues/toyos-ssh-replays-rest-on-rings-deprecated-test-randomness.md`.

## Stages

1. **The core, on the host.** `toyos-ssh`: strict `curve25519-sha256`, the
   `ssh-ed25519` host key from sshserver's existing `openssh-key-v1` file,
   `chacha20-poly1305@openssh.com`, `publickey` authentication with Ed25519
   keys, and `session` channels serving `exec`. Its tests are OpenSSH_10.3p1
   recordings replayed byte for byte, RFC vectors and a structured fuzz, under
   `cargo run -- --ci host`. Landed by #817. Nothing but its tests calls
   `Server` or the connection layer until stage 2.
2. **The switch and the deletion.** `userland/sshserver` is rewritten on
   `toyos-ssh` with `std` threads, keeping what it serves today: a shell over
   pipes, `subsystem sftp` over the existing `sftp.rs`, and `exec` through
   `command.rs`. It adds an `authorized_keys` reader, several channels on one
   connection, a login deadline and a cap on unauthenticated connections. The
   login deadline is what bounds a client that only offers keys, because an
   offer answered `PK_OK` counts as no failure. This stage closes
   `issues/sshserver-holds-one-channel-and-does-not-say-so.md` and
   `issues/sshserver-still-speaks-rsa.md`. It waits for stage 3, and for
   `ring` to link for both ToyOS targets
   (`issues/the-internet-clients-work-unchanged.md`, stage 3).
   **Exit**:
   - `userland/sshserver/Cargo.toml` names `toyos-ssh`, and names neither
     `russh` nor `tokio`.
   - The root `[patch.crates-io]` has no `russh`, `russh-cryptovec` or `tokio`
     entry.
   - `Cargo.lock` has no `russh`, `russh-cryptovec`, `rsa`, `pageant` or
     `internal-russh-num-bigint` package.
   - A recorded `sftp` session and a recorded shell session replay byte for
     byte under `cargo run -- --ci host`.
3. **ML-KEM.** Our own `mlkem768x25519-sha256`: the server's encapsulation
   side, as one more `kex::METHODS` entry. `ring` 0.17.14 has no ML-KEM.
   Lands before stage 2.
   **Exit**:
   - NIST's ML-KEM-768 encapsulation known answers pass.
   - An OpenSSH recording that negotiated `mlkem768x25519-sha256` replays
     byte for byte, and its client's log has no post-quantum warning.
   - `issues/toyos-ssh-offers-only-a-classical-key-exchange.md` is closed.
4. **The forks retire.** After stage 2, tokio has no consumer in the
   workspace, and mio and socket2 remain only as host dev-dependencies of
   `userland/netstack/node`.
   **Exit**:
   - The root `[patch.crates-io]` names none of `ToyOSOrg/tokio`,
     `ToyOSOrg/mio` and `ToyOSOrg/socket2`.
   - `issues/the-tokio-forks-root-manifest-patches-mio.md` is closed.
5. **On the T14.** A metal row logs in to sshserver, runs an `exec`, and moves
   100 MB of standard input whose hash is checked. Which client the row drives
   is not decided.
   **Exit**: the row is in the metal set and green.
