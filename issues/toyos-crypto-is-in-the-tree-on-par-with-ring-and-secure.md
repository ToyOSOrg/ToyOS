---
status: open
kind: track
opened: 2026-10-13
---

# toyos-crypto is in the tree, on par with ring, and secure

Owner goal (2026-10-13): adopt `toyos-crypto`, the crate Codex wrote for
ToyOS outside the tree, own it here, and bring it into this repository. The
bar is at least ring's speed, and secure. It is pure Rust and `no_std`: hashes,
HMAC/HKDF, AES-GCM, ChaCha20-Poly1305, OpenSSH's ChaCha20-Poly1305, X25519,
Ed25519, P-256/P-384 ECDH and ECDSA verification, RSA verification, ML-KEM-768,
WPA2's primitives and an optional rustls provider. It has no entropy source;
the caller supplies one through its `Random` trait. Its README says that it is
experimental, that no independent arithmetic review has been done, and that
passing vectors does not show it is safer than ring.

**Owner**: the orchestrator. The crate lives at `lib/crypto`, package
`toyos-crypto`. Its ARM paths have native macOS correctness tests, but no ARM
metal machine can give them a timing verdict.

## Where it stands

- Source: `toyos-crypto` commit `2b3f92c0dc6a7b77aaeff57d1291cab8dba78494`,
  a separate repository with no remote. The working tree has uncommitted
  AES-GCM work.
- On the benches under Linux, comparing 16 KiB seal against stock ring with
  its runtime dispatch:
  - AES-128/256-GCM is 1.80x/1.67x slower on the T14 (VAES) and 1.68x/1.76x
    slower on the AMD bench.
  - ChaCha20-Poly1305 is 2.79x/3.38x slower.
- On the development Mac, an exploratory and uncontrolled run under load:
  - SHA-256 is 7–26x slower than ring; the crate has no SHA instructions.
  - X25519 is 5–6x slower.
  - P-256 ECDH, Ed25519 sign and verify, ECDSA P-256 verify and RSA-2048
    verify are 14–190x slower.
  - No metal bench has measured these; the AEAD comparator covers only the
    AEADs.
- Inside ToyOS today `CR4.OSXSAVE` is clear
  (`issues/user-programs-use-avx-under-xsave.md`), so both the crate and ring
  run their SSE paths there. The crate's only vector ChaCha path is AVX2, so
  its ChaCha is scalar in ToyOS.

## Consumers and what each needs

| Consumer | Today | Needs from the crate |
|---|---|---|
| `toyos-ssh` (`build/ssh`), host-only until `issues/the-ssh-server-is-toyos-own-and-russh-is-gone.md` stage 2 | `ring` | X25519, ML-KEM-768 and its hybrid, Ed25519 sign/verify, `chacha20-poly1305@openssh.com`, SHA-256/512, and a caller-supplied `Random` for replays |
| `system/sshserver` | russh with the RustCrypto stack, including `rsa` (RUSTSEC-2023-0071) | none directly; it reaches the crate through `toyos-ssh` |
| Image and package signing: `lib/update` (loader, `/system/bin/update`, `build/signing.rs`, `build/root`, #892's repository keys) | `ed25519-dalek` `verify_strict`; `toyos-sha2` SHA-512; loader `sha2` `force-soft` | strict Ed25519 verification in the soft-float UEFI loader; signing behind `sign` on the host |
| Every SHA-256: build, `pkg`, `lib/swap`, `system/update` | `toyos-sha2` / `toyos-sha2-hw` (SHA-NI on x86-64, scalar on AArch64) | one SHA-2 implementation, no slower than `toyos-sha2-hw` |
| ToyOS's own HTTPS clients: on the host, the build's `ureq` (`build/release.rs`) and doom's build script (`apps/doom/build.rs`). In a guest, none today: `https_tls13` and `pkg install <url>` are stages 3 and 5 of `issues/the-internet-clients-work-unchanged.md` | rustls on `ring`, which is `ToyOSOrg/ring` in a guest | the rustls provider through ureq's `rustls-no-provider` feature |
| Wi-Fi's `wlan` (`issues/there-is-no-wifi.md` P6, W2) | nothing yet | PBKDF2-HMAC-SHA1, the 802.11i PRF and RFC 3394 unwrap, which the crate has; AES-CMAC-128, SAE over P-256 by hash-to-element, a public constant-time comparison and a public wiping secret, which it lacks |
| `issues/programs-hold-aes-keys-as-key-locker-handles.md` | blocked on a session-long AES key | TLS or SSH on the crate is that consumer; AES behind a Key Locker handle |

Not consumers:
- The kernel's generator (`lib/random`), by the owner's ruling.
- Unmodified third-party programs. A published crate that selects ring still
  builds on `ToyOSOrg/ring`. That fork stays as platform support for them.

## Stages

1. **The move.** `lib/crypto` is a workspace member holding the source
   commit's library, tests and vectors. The commit message names that commit,
   and its full history is published on `ToyOSOrg/evidence`. The source
   repository then stops changing: this tree becomes the crate's only source.
   `toyos-ssh` switches from `ring` in the same landing.
   **Exit**:
   - The crate's host tests run under `cargo run -- --ci host` in its
     dispatched, `portable` and default-feature forms.
   - Every vector file has a `NOTICE` entry and a `COMMITTED_FILES` row.
   - The tree has none of the source's `evidence/`, `tools/*.py` or
     `claude-comms/`.
   - `build/ssh/Cargo.toml` names no `ring`.
   - `issues/toyos-ssh-replays-rest-on-rings-deprecated-test-randomness.md` is
     closed with the recordings replaying byte for byte.
2. **The instrument.** The ring comparator becomes a dev-only program in
   `lib/crypto`, with `ring` as a dev-dependency. It covers every hot-path
   operation a consumer calls, at matched sizes:
   - AEAD seal, open, rejection and key setup, from 0 B to 16 KiB, including
     SSH's packet sizes;
   - SHA-2, HMAC and HKDF;
   - X25519, P-256/P-384 ECDH, Ed25519 sign and verify, ECDSA verify and RSA
     2048/4096 verify.
   It runs in a ToyOS guest and under Linux on the T14 and the AMD bench, with
   interleaved samples. ML-KEM is reported beside RustCrypto's `ml-kem`, since
   ring has none.
   **Exit**: both benches' tables are on `ToyOSOrg/evidence`. Each row has
   ours/ring, the medians and the ring-against-ring spread from the same boot.
3. **Parity.** **Exit**: in a ToyOS guest on both benches, every hot-path row's
   median ours/ring ratio is at most 1 plus that row's ring-against-ring
   spread. When `issues/user-programs-use-avx-under-xsave.md` lands, the rows
   are run again with both crates admitting AVX. Linux runs are reported and
   decide nothing. Faster code is accepted only with its stage 4 evidence.
4. **Secure.** **Exit**, for each primitive a consumer uses:
   - **Independent vectors**: every Wycheproof file for a supported
     interface, and NIST ACVP or CAVP for AES-GCM, SHA-2 (`tests/cavp`),
     HMAC, ECDSA and ML-KEM. Each case outside the interface is counted as
     refused.
   - **Differential tests** against ring and the RustCrypto crates, as
     dev-dependencies: seeded, a fixed count, under `--ci host`.
   - **A structured fuzz** of every decoder: SEC 1 points, DER signatures,
     RSA components, ML-KEM keys and ciphertexts, and the provider's inputs.
   - **Constant time where secrets flow.** Each secret routine's module
     contract names its secret inputs. On both benches, a ToyOS guest and the
     shipped compiler, a timing test produces a Welch t verdict, and a
     deliberately leaky control must be detected. The emitted code is also
     inspected for secret-dependent branches and addresses on
     `x86_64-unknown-toyos`, `aarch64-unknown-toyos` and the UEFI loader
     target.
   - **Negative controls**: each check a forgery would pass (tag compare,
     point and range validation, S < L, the lengths) is mutated as a checked
     patch, and its exact test fails. No mutation runner is committed.
   - **Independent arithmetic review**: an agent's review is not
     independent. Each multi-precision module needs both of these:
     - its limb and residue bounds stated as debug assertions, which the
       guest profile and `--ci host` keep on;
     - its field and scalar operations differentially tested against
       `fiat-crypto`, whose machine-checked arithmetic is a dev-dependency
       oracle.
     A human cryptographer's audit is the owner's decision.
5. **Consumers switch.** A consumer that ships switches only after stages 3
   and 4 hold for every primitive it uses, and its old dependency is deleted
   in the same landing:
   - `toyos-ssh`, which ships nothing, may offer `mlkem768x25519-sha256`
     first at any time after stage 1
     (`issues/toyos-ssh-offers-only-a-classical-key-exchange.md`).
     sshserver's switch, stage 2 of the SSH track, is the shipping step: it
     waits for SSH's primitives, and no longer waits for `ring` on either
     ToyOS target.
   - `toyos-sha2` and `toyos-sha2-hw` are folded into the crate's SHA-2, which
     gains the SHA instructions of x86-64 and AArch64. Their SHAVS tests come
     too.
   - `lib/update` verifies and signs with the crate. Its edge corpus matches
     `ed25519-dalek`'s `verify_strict`. A loader on each bench verifies a
     signed image. `ed25519-dalek` and the loader's `sha2` leave
     `Cargo.lock`'s non-dev packages.
   - ToyOS's HTTPS clients use the provider. The `https_tls13` metal row
     passes on it. The `cc` row of
     `issues/the-build-runs-host-tools-outside-rust-and-qemu.md` no longer
     names ring for the build system.
   - Wi-Fi's P6 is this crate's AES-CMAC, SAE by hash-to-element, public
     constant-time comparison and public wiping secret, each against its
     RFC or Annex J vectors.

## Weaknesses until the exits hold

- Owned secret storage is wiped on drop. Compiler copies, registers, spills
  and caller-held bytes are not, as the crate's README states.
- **Owner**: the orchestrator. **Exit**: stage 4's review states what the
  shipped targets keep, and a consumer holding a long-lived key stores it
  under a wiping owner or a Key Locker handle.
