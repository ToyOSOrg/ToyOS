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
the caller supplies one through its `Random` trait. No independent arithmetic
review has been done, and passing vectors does not show it is safer than ring.

**Owner**: the orchestrator. The crate lives at `lib/crypto`, package
`toyos-crypto`.

## Where it stands

- **Frozen source.** The source is commit
  `2b3f92c0dc6a7b77aaeff57d1291cab8dba78494` (owner, 2026-10-13). Its
  uncommitted files are not imported. The source of record is
  `ToyOSOrg/evidence` branch `toyos-crypto-2b3f92c`, which holds its full
  history
  (https://github.com/ToyOSOrg/evidence/tree/2b3f92c0dc6a7b77aaeff57d1291cab8dba78494).
- **Linux on the benches, 16 KiB seal against stock ring with its runtime
  dispatch:**
  - AES-128/256-GCM is 1.80x/1.67x slower on the T14 (VAES) and 1.68x/1.76x
    slower on the AMD bench (archive `evidence/current/vaes-native/README.md`).
  - ChaCha20-Poly1305 is 2.79x/3.38x slower
    (`evidence/current/avx2-native/`).
- **Development Mac, exploratory and uncontrolled, under load:**
  - SHA-256 is 7–26x slower than ring; the crate has no SHA instructions.
  - X25519 is 5–6x slower.
  - P-256 ECDH, Ed25519 sign and verify, ECDSA P-256 verify and RSA-2048
    verify are 14–190x slower.
  - Evidence: `ToyOSOrg/evidence` `pr-913`,
    https://github.com/ToyOSOrg/evidence/tree/b7cde8091500ac0f7773c56fd0d70c8bbe9429aa/pr-913/exploratory-mac-asym.
  - No metal bench has measured these.
- **Inside ToyOS `CR4.OSXSAVE` is set** since f7b497231. The kernel's cpu0
  records in #932's bench boots show XCR0 0xe7 on the T14 (AVX-512 state)
  and 0x7 on the AMD bench (AVX). The parity instrument records what
  feature detection admits there, AVX2 and AES-NI on both benches and
  VAES/AVX-512 on the T14, so both crates' vector paths are open.
- **AArch64 is scalar.** No ARM bench exists to measure an ARM path, so the
  crate's AArch64 AES and PMULL paths are not imported. They stay in the
  archive until an ARM bench measures them. `toyos-sha2-hw` follows the same
  rule.

## Consumers and what each needs

| Consumer | Today | Needs from the crate |
|---|---|---|
| `toyos-ssh`'s SSH protocol (`build/ssh`: `kex`, the server, the client). Host-only until `issues/the-ssh-server-is-toyos-own-and-russh-is-gone.md` stage 2 | the crate, since stage 1 | X25519, ML-KEM-768 and its hybrid, Ed25519 sign/verify, `chacha20-poly1305@openssh.com`, SHA-256/512, and a caller-supplied `Random` for replays |
| `toyos-ssh`'s `hostkey` on the signing path. `build/signing.rs` reads the owner's image-signing key through `openssh_seed` and writes a minted one through `openssh_private`; both derive the public key from the seed | `toyos_update::sig::public_of`, since stage 1 | nothing: this derivation is the root of trust's; the verifier itself switches in stage 6 |
| `system/sshserver` | russh with the RustCrypto stack, including `rsa` (RUSTSEC-2023-0071) | none directly; it reaches the crate through `toyos-ssh` |
| Image and package signing: `lib/update` (the loader, `/system/bin/update`, `build/signing.rs`, `build/root`, #892's repository keys) | `ed25519-dalek` `verify_strict`; `toyos-sha2` SHA-512; the loader's `sha2` with `force-soft` | strict Ed25519 verification in the soft-float UEFI loader, and signing behind `sign` on the host. The track's last stage, stage 6 (owner, 2026-10-13) |
| Every SHA-256: the build, `pkg`, `lib/swap`, `system/update` | `toyos-sha2` / `toyos-sha2-hw` (SHA-NI on x86-64, scalar on AArch64) | one SHA-2 implementation, no slower than `toyos-sha2-hw` |
| ToyOS's own HTTPS clients. On the host: the build's `ureq` (`build/release.rs`) and doom's build script (`apps/doom/build.rs`). In a guest, none today: `https_tls13` and `pkg install <url>` are stages 3 and 5 of `issues/the-internet-clients-work-unchanged.md` | rustls on `ring`, which is `ToyOSOrg/ring` in a guest | the rustls provider through ureq's `rustls-no-provider` feature. Stage 5, once parity and security hold (owner, 2026-10-13) |
| Wi-Fi's `wlan` (`issues/there-is-no-wifi.md` P6, W2) | nothing yet | PBKDF2-HMAC-SHA1, the 802.11i PRF and RFC 3394 unwrap, which the crate has. AES-CMAC-128, SAE over P-256 by hash-to-element, a public constant-time comparison and a public wiping secret, which it lacks |
| `issues/programs-hold-aes-keys-as-key-locker-handles.md` | blocked on a session-long AES key | a TLS client on the crate is that consumer: AES behind a Key Locker handle. SSH holds no AES key |

Not consumers:
- The kernel's generator (`lib/random`), by the owner's ruling.
- Unmodified third-party programs. A published crate that selects ring still
  builds on `ToyOSOrg/ring`, which stays as a fork.

## Stages

1. **The move, with `toyos-ssh`'s protocol as its first consumer.** One
   landing:
   - Its first commit adds `lib/crypto` from the frozen commit and names
     it. Only the crate's `src/` (without `src/arch/aarch64.rs`), `tests/`,
     `tests/data/` and that data's licence files enter.
     - The same commit removes the AArch64 dispatch and
       `cpu::initialize_aarch64*`, so that it compiles for AArch64.
     - It writes the crate's manifest with caret requirements.
   - Nothing else comes in: no `examples/` (the mutation runner and the timing
     harness), `benchmarks/`, `evidence/`, `tools/`, `Cargo.lock`,
     `LICENSE-*`, `NOTICE`, `README.md` or `VALIDATION.md`. They stay in the
     archive.
   - Later commits in the same landing do the following:
     - Replace the `rsa` oracle:
       - `src/test_arithmetic.rs`'s integers come from `num-bigint`;
       - the RSA differential signs with ring, from committed PKCS#8 test
         keys;
       - the provider's RSA certificates use those keys.
     - Switch `toyos-ssh`'s protocol from `ring`. The public-key derivation
       in `hostkey::openssh_seed` and `openssh_private` calls
       `toyos_update::sig::public_of` instead of ring.
   **Exit**:
   - Under `cargo run -- --ci host`, the crate's host tests run in its
     dispatched, `portable` and default-feature forms.
   - Every vector file has a `NOTICE` entry and a `COMMITTED_FILES` row.
   - `Cargo.lock` has no `rsa` package that the crate brings in. There is no
     `rsa` crate in the crate's dependencies of any kind.
   - `build/ssh/Cargo.toml` names no `ring`.
   - `hostkey`'s derivation calls no `lib/crypto` code. `build/signing.rs`
     derives public keys only through `toyos_update::sig::public_of`.
   - `toyos-ssh`'s OpenSSH recordings replay byte for byte, and the issue
     that its replays rest on ring's deprecated test randomness is closed.
   - These issues no longer name ring as a dependency:
     - `issues/the-ssh-server-is-toyos-own-and-russh-is-gone.md` (stage 2's
       wait for ring to link on both ToyOS targets);
     - `issues/toyos-ssh-leaves-its-secrets-in-freed-memory.md`.
2. **The instrument.** The ring comparator is a guest program in
   `tests/guest/rust-tests`, the workspace that already patches ring, with
   `ring` as its oracle.
   - It covers every hot-path operation a consumer calls:
     - AEAD seal, open, rejection and key setup at 0, 64, 1024 and 16384 B,
       and SSH's 32768 B packet;
     - SHA-256/512 at 64 B and 16 KiB, HMAC and HKDF;
     - X25519, P-256/P-384 ECDH, Ed25519 sign and verify, ECDSA verify and RSA
       2048/4096 verify.
   - Its rows are metal rows on the T14 and the AMD bench, in one shared boot
     per bench with `ring_kat`. Ring has never run on ToyOS metal, so
     `ring_kat` is the row that establishes that it runs at all.
   - Nothing runs under Linux.
   - ML-KEM rows are excluded from the verdict by name, because ring has no
     ML-KEM. They are reported beside RustCrypto's `ml-kem`.
   - `ring::test` stays in `tests/guest/rust-tests/src/bin/ring_kat.rs`, as
     ring's own known-answer row: it is the one way to give ring a known
     X25519 key.
   - The program is `crypto_parity`, built a second time with debug
     assertions off as `crypto_parity_unasserted`. `lib/parity` holds the
     rule. The metal row `crypto_instrument` prints both builds' table in the
     boot `cryptocase`.
   **Exit**: both benches' tables are on `ToyOSOrg/evidence`, with the
   columns stage 3 defines.
3. **Parity.**
   - **The verdict's row is this stage's.** `toyos_parity::Table::verdict`
     is the rule. Its metal row, judged on `cryptocase` beside
     `crypto_instrument`, is registered when this stage's rows pass.
   - **Profile.** The verdict is taken in `[profile.toyos]`, as every guest
     binary ships: `opt-level = 2`, with debug assertions and overflow checks
     on. The crate's assertions are part of what ships, and their cost
     counts. Each row also reports the same build with debug assertions off,
     as a column that decides nothing, so that their cost is visible.
   - **Samples.** Ring runs as two interleaved instances, A and B, beside
     ours, in the same boot. The order of ours, A and B rotates from one
     sample to the next. A row is 3 rounds of 7 paired samples, after one
     discarded warmup sample, so 21 samples per backend. A sample is the mean
     over a batch of at least 1 ms.
   - **Spread.** For sample i, r_i = A_i / B_i. A row's spread is the 90th
     percentile of |r_i − 1| over its 21 samples. A row whose spread exceeds
     5% gets no verdict and is measured again.
   - **Verdict.** A row passes when the median over i of
     ours_i / ((A_i + B_i) / 2) is at most 1 + spread.
   - **A recorded bias.** An open row restores its ciphertext before each
     open, and every backend pays for that copy, which pulls the ratio
     toward 1. A pass on an open row is read with that bias in mind. On the
     development Mac (exploratory), the copy is 3–6.5% of ring's AES-GCM open
     and 1–1.7% of its ChaCha20-Poly1305 open. No bench has measured it.
   - **Exit:**
     - On both benches, every row a consumer uses passes on the ISA each
       build records. That ISA is now AVX-capable state on both benches.
     - Parity work reads ring's, BoringSSL's and OpenSSL's perlasm or C only
       through a clean room, as root's research rule says.
     - Faster code is accepted only with stage 4's evidence for it.
   - **First measurement on the benches**, #932 at 85cda28d3, shipped
     build. Each row was built only after ours matched ring on its inputs,
     except the three ECDH rows: their check, an agreement of ours and ring
     on one secret, came after this run.
     A family's ratio is ours over ring's mean, and its median is the lower
     middle of the family's rows. Each build recorded its ISA:
     - the T14: `aes,pclmulqdq,sha,movbe,bmi2,adx,avx,avx2,vaes,vpclmulqdq,avx512f,avx512vl`;
     - the AMD bench: `aes,pclmulqdq,sha,movbe,bmi2,adx,avx,avx2`.

     | family | T14 median | T14 worst | AMD median | AMD worst |
     |---|---|---|---|---|
     | AES-GCM (26 rows) | 1.43 | 3.63 key setup | 1.68 | 2.36 seal 16 KiB |
     | ChaCha20-Poly1305 (13) | 4.74 | 6.98 seal 16 KiB | 4.68 | 8.74 seal 16 KiB |
     | chacha20-poly1305@openssh (16) | 2.82 | 13.5 key setup | 2.94 | 19.0 key setup |
     | SHA-2 (4) | 1.96 | 7.25 SHA-256 16 KiB | 1.73 | 7.85 SHA-256 16 KiB |
     | HMAC/HKDF (4) | 1.94 | 3.39 HMAC-SHA-256 | 1.80 | 3.98 HMAC-SHA-256 |
     | ECDH (3) | 13.3 | 21.2 P-256 | 10.6 | 22.2 P-256 |
     | Ed25519 sign, verify | 70.8, 34.3 | | 59.7, 25.3 | |
     | ECDSA verify P-256, P-384 | 22.1, 4.92 | | 23.3, 5.39 | |
     | RSA verify (4) | 30.6 | 38.2 2048 PKCS#1 | 27.6 | 34.6 2048 PKCS#1 |
     | ML-KEM-768 beside `ml-kem`, excluded | 1.01 | 1.08 | 1.05 | 1.11 |

     The T14 has 70 fails, 1 remeasure and 3 passes. The AMD bench has 73
     fails and 1 pass. The first run, at 9a318695e, recorded no ISA and
     printed samples rounded to 0.1 ns. Its figures agree with these within
     about 5%. Tables:
     https://github.com/ToyOSOrg/evidence/tree/4dfa4dc8c647bee3424a13be28191bdb2213bcbd/pr-932/r2.
   - **First measurement, not a verdict.** `toyos-ssh`'s fuzz
     (`build/ssh/tests/fuzz.rs`), on the crate since stage 1: 33.92 s and
     15.46 s in two runs on the development Mac (an M4 Pro). On ring it took
     1.27 s in main's nightly host job, on GitHub's runner.
4. **Secure.** **Exit**, for each primitive a consumer uses:
   - **Typed domains.** The private `V<N>` is replaced by distinct types:
     integers, Montgomery residues per modulus, field elements and scalars per
     curve. Passing one where another is meant does not compile. A secret
     owner's in-place initialisation is enforced by its type rather than by
     call order. Debug assertions remain only for the limb bounds types cannot
     express.
   - **Machine-checked arithmetic as oracle.** These modules are differentially
     tested against `fiat-crypto` (`curve25519_64`, `curve25519_scalar_64`,
     `p256_64`, `p256_scalar_64`, `p384_64`, `p384_scalar_64`,
     `poly1305_64`), as a dev-dependency:
     - `field25519` and `scalar25519`;
     - P-256/P-384's field and scalar arithmetic;
     - Poly1305.

     The rest is covered as follows:
     - ML-KEM's modulo-3329 reduction is tested exhaustively over its input
       range.
     - RSA is public-only. Wycheproof and the ring differential cover it.
   - **Independent vectors.** Every Wycheproof file for a supported
     interface, and NIST ACVP or CAVP for AES-GCM, SHA-2 (`tests/cavp`),
     HMAC, ECDSA and ML-KEM. Each case outside the interface is counted as
     refused.
   - **Differential tests** against ring and the RustCrypto crates (no `rsa`
     crate), as dev-dependencies: seeded, 10,000 cases per primitive, under
     `--ci host`.
   - **A structured fuzz** of every decoder: SEC 1 points, DER signatures,
     RSA components, ML-KEM keys and ciphertexts, and the provider's inputs.
     It is seeded and runs 4,000 cases per decoder under `--ci host`, as
     `build/ssh/tests/fuzz.rs` does.
   - **Constant time where secrets flow.** Each secret routine's module
     contract names its secret inputs. For each such routine:
     - **Classes.** Fixed-versus-random: one class holds a fixed secret, the
       other a fresh uniform secret per measurement, and the public inputs are
       the same in both.
     - **The fixed secret.** It is a special value: the scalar 1 for scalar
       multiplication, the all-zero key for ciphers and MACs, and otherwise
       the lowest-weight valid secret. A special value exposes
       data-dependent paths that a typical value does not.
     - **Samples.** 1,000,000 measurements per routine and bench, with the
       class chosen at random per measurement. They are taken in a ToyOS guest
       in `[profile.toyos]`, on the T14 and the AMD bench.
     - **Threshold.** Pass when the maximum Welch |t| is below 4.5, over the
       raw and the percentile-cropped sets.
     - **Control.** In the same run, a deliberately leaky control (a
       secret-dependent branch) must exceed |t| = 10.
     - **Code inspection.** The emitted code is inspected for secret-dependent
       branches and addresses on `x86_64-unknown-toyos`,
       `aarch64-unknown-toyos` and the UEFI loader target.
     - **Record.** The result is evidence recorded in this issue, with its
       run, revision and compiler. It is not a suite row, and it is rerun when
       the routine or the compiler changes.
   - **Negative controls.** Each check a forgery would pass is mutated as a
       checked patch, and its exact test fails: the tag compare, point and
       range validation, S < L, and the lengths. No mutation runner is
       committed.
   - **`unsafe`.** Miri runs the `portable` configuration's unit tests on the
     host, covering the volatile wipes and the `ct` barriers. The run is
     recorded as evidence.
   - **Audit.** Whether a human cryptographer audits the crate is the owner's
     decision, and it is open.
5. **Consumers switch.** Rules for every consumer:
   - A consumer on ring today switches only after stages 3 and 4 hold for
     every primitive it uses.
   - A consumer that ships no crypto today switches after stage 4 holds for
     its primitives. Parity for those primitives is a later stage, not a
     gate.
   - Each consumer's old dependency is deleted in the same landing.
   - The NIST ML-KEM vectors in `lib/crypto/tests/data` are under
     `NIST-Software`, which the licence allowlist (`build/licence.rs`) does
     not name. It decides that licence before `lib/crypto` enters an image.

   The consumers:
   - `toyos-ssh`'s protocol offers `mlkem768x25519-sha256` first, at any time
     after stage 1
     (`issues/toyos-ssh-offers-only-a-classical-key-exchange.md`). sshserver's
     switch, stage 2 of the SSH track, is the shipping step. It waits for
     stage 4 on SSH's primitives.
   - The crate's SHA-2 gains x86-64's SHA instructions; AArch64 stays scalar.
     These users of `toyos-sha2` and `toyos-sha2-hw` switch to the crate:
     `build/`'s digests, `pkg` and `lib/swap`. Three keep them until stage 6:
     - `lib/update`;
     - the loader;
     - `system/update`, whose `stream_root` holds ROOT to the signed header's
       digest.
   - ToyOS's own HTTPS clients use the crate's rustls provider once stages 3
     and 4 hold for TLS's primitives. The owner ruled this on 2026-10-13:
     the crypto goal overrides his ruling of 2026-10-02 in
     `issues/the-internet-clients-work-unchanged.md` stage 3. Until then ring
     stays the provider. `ToyOSOrg/ring` stays as a fork so that unmodified
     programs build.
     - The `https_tls13` metal row passes on it.
     - The `cc` row of
       `issues/the-build-runs-host-tools-outside-rust-and-qemu.md` stops
       naming ring for the build system's HTTP agent. It still names ring as
       `lib/crypto`'s dev-dependency oracle, whose C and assembly every
       `--ci host` run compiles.
   - Wi-Fi's P6 is this crate's AES-CMAC, SAE by hash-to-element, public
     constant-time comparison and public wiping secret, each against its RFC
     or Annex J vectors.
6. **The root of trust, last.** The loader and `lib/update` verify, and the
   host signs, with the crate. The owner ruled on 2026-10-13 that this is the
   track's last stage, taken only after every stage 4 exit holds.
   **Exit**:
   - Ring stays a host differential oracle of the verifier.
   - The edge corpus is Wycheproof's `ed25519` and the vectors of Chalkias
     et al., "Taming the many EdDSAs" (2020). On both corpora the crate agrees
     with `ed25519-dalek`'s `verify_strict` and with ring.
   - The loader verifies a signed image on the T14 and the AMD bench, with
     its verification time measured on each.
   - An AArch64 QEMU loader boot verifies a signed image, because the crate's
     UEFI path has never run.
   - `lib/update`, the loader and `system/update` hash with the crate's SHA-2. `toyos-sha2`
     and `toyos-sha2-hw` are deleted, and their SHAVS tests move to the crate.
   - `ed25519-dalek` and the loader's `sha2` leave `Cargo.lock`'s non-dev
     packages.

## Weaknesses until the exits hold

- Owned secret storage is wiped on drop. Compiler copies, registers, spills
  and caller-held bytes are not wiped.
- **Owner**: the orchestrator. **Exit**: stage 4 states what the shipped
  targets keep, and a consumer holding a long-lived key stores it under a
  wiping owner or a Key Locker handle.
