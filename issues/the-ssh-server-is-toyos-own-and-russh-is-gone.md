---
status: open
kind: track
opened: 2026-10-09
---

# The SSH server is ToyOS's own, and russh is gone

`system/sshserver` serves SSH on the `russh` fork over `tokio`. The root
`[patch.crates-io]` carries `russh`, `russh-cryptovec` and `tokio` for it, and
`Cargo.lock` carries the closure they bring: `rsa` (RUSTSEC-2023-0071, the
Marvin attack), `pageant` and `internal-russh-num-bigint` among it. This track
puts the server on `toyos-ssh`, ToyOS's own sans-IO server core on
`toyos-crypto` (`lib/crypto`), under a `std` driver, and deletes the russh closure in the landing that
switches.

**Owner**: `toyos-ssh` and `system/sshserver`.

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
`issues/toyos-ssh-offers-only-a-classical-key-exchange.md` and
`issues/toyos-ssh-leaves-its-secrets-in-freed-memory.md`.

A compromise of the server shipped today, until stage 2: russh's `p384`
runs on its `bignum` backend (`--cfg=p384_backend="bignum"` in
`.cargo/config.toml`), which keeps fiat-crypto out under the owner's ruling
that ToyOS takes it as a dependency of no kind. `p384` 0.14.0-rc.8 calls that
backend experimental and says it "might also have bugs", where fiat's is
formally verified (`src/lib.rs`). sshserver reaches P-384 in two places:
it verifies a client's `ecdsa-sha2-nistp384` publickey signature, which is
untrusted input before authentication, and it signs with a P-384 host key
when the operator's host-key file holds one (a minted key is Ed25519). It
negotiates no `ecdh-sha2-nistp384`: russh's default order leaves it out.
Owner: this track's. Exit: stage 2, which deletes russh and the setting.

## Stages

1. **The core, on the host.** `toyos-ssh`: strict `curve25519-sha256`, the
   `ssh-ed25519` host key from sshserver's existing `openssh-key-v1` file,
   `chacha20-poly1305@openssh.com`, `publickey` authentication with Ed25519
   keys, and `session` channels serving `exec`. Its tests are OpenSSH_10.3p1
   recordings replayed byte for byte, RFC vectors and a structured fuzz, under
   `cargo run -- --ci host`. Landed by #817. Nothing but its tests calls
   `Server` or the connection layer until stage 2.
2. **The switch and the deletion.** `system/sshserver` is rewritten on
   `toyos-ssh` with `std` threads, keeping what it serves today: a shell over
   pipes, `subsystem sftp` over the existing `sftp.rs`, and `exec` through
   `command.rs`. It adds an `authorized_keys` reader, several channels on one
   connection, a login deadline and a cap on unauthenticated connections. The
   login deadline is what bounds a client that only offers keys, because an
   offer answered `PK_OK` counts as no failure. This stage closes
   `issues/sshserver-holds-one-channel-and-does-not-say-so.md` and
   `issues/sshserver-still-speaks-rsa.md`. It waits for stage 3, and for
   stage 4 of `issues/toyos-crypto-is-in-the-tree-on-par-with-ring-and-secure.md`
   on SSH's primitives.
   **Exit**:
   - `system/sshserver/Cargo.toml` names `toyos-ssh`, and names neither
     `russh` nor `tokio`.
   - The root `[patch.crates-io]` has no `russh`, `russh-cryptovec` or `tokio`
     entry.
   - `Cargo.lock` has no `russh`, `russh-cryptovec`, `rsa`, `pageant` or
     `internal-russh-num-bigint` package.
   - `.cargo/config.toml` sets no `p384_backend`.
   - A recorded `sftp` session and a recorded shell session replay byte for
     byte under `cargo run -- --ci host`.
3. **ML-KEM.** Our own `mlkem768x25519-sha256`, on `toyos-crypto`'s
   ML-KEM-768 and its hybrid: the server's encapsulation side, as one more
   `kex::METHODS` entry. Lands before stage 2.
   **Exit**:
   - NIST's ML-KEM-768 encapsulation known answers pass.
   - An OpenSSH recording that negotiated `mlkem768x25519-sha256` replays
     byte for byte, and its client's log has no post-quantum warning.
   - `issues/toyos-ssh-offers-only-a-classical-key-exchange.md` is closed.
4. **The forks retire.** After stage 2, tokio has no consumer in the
   workspace, and mio and socket2 remain only as host dev-dependencies of
   `system/netstack/node`.
   **Exit**:
   - The root `[patch.crates-io]` names none of `ToyOSOrg/tokio`,
     `ToyOSOrg/mio` and `ToyOSOrg/socket2`.
   - `issues/the-tokio-forks-root-manifest-patches-mio.md` is closed.
5. **On the T14.** A metal row logs in to sshserver, runs an `exec`, and moves
   100 MB of standard input whose hash is checked. Which client the row drives
   is not decided.
   **Exit**: the row is in the metal set and green.
