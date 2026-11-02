---
status: open
kind: track
opened: 2026-10-13
---

# toyos-crypto is in the tree, on par with ring, and secure

Owner goal (2026-10-13): adopt `toyos-crypto`, the crate Codex wrote for
ToyOS outside the tree, own it here, and bring it into this repository. The
bar is at least ring's speed, and secure. It is pure Rust and `no_std`: hashes,
HMAC/HKDF, PBKDF2, scrypt, AES-GCM, ChaCha20-Poly1305, OpenSSH's ChaCha20-Poly1305, X25519,
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
- **AArch64 is scalar but for SHA-2.** No ARM bench exists to measure an ARM
  path, so the crate's AArch64 AES and PMULL paths are not imported. They
  stay in the archive until an ARM bench measures them. SHA-256 and SHA-512
  run on the Armv8 SHA instructions where the target promises them, by the
  owner's word (2026-10-22): the development Mac's, where the build hashes
  every image. There (exploratory, an M4 Pro) SHA-256 of 1 MiB runs at
  2.6 GB/s against the portable rounds' 0.52, and SHA-512 at 1.36 against
  0.83. ToyOS's own AArch64 targets do not promise them and nothing detects
  them at run time, so they hash portably; see the weaknesses.
- **On ToyOS every operation that takes a secret runs under the thread's
  timing protection** (`src/timing.rs`, `toyos::timing`): `PSTATE.DIT` on
  AArch64. That covers ciphers, MACs, KDFs, keyed SipHash, `ct::equal` and
  the key operations. A plain hash takes none, so a caller hashing a secret
  takes `toyos_crypto::timing::protect` itself, as `toyos-ssh`'s exchange
  hash and key derivation do. x86-64 has no such mode a thread sets, and
  ToyOS turns Intel's DOITM on for none, so constant time there rests on the
  code, as on AMD.

## Consumers and what each needs

| Consumer | Today | Needs from the crate |
|---|---|---|
| `toyos-ssh`'s SSH protocol (`lib/ssh`: `kex`, the server, the client). In the image through `system/sshserver` since `issues/the-ssh-server-is-toyos-own-and-russh-is-gone.md` stage 2 | the crate, since stage 1 | X25519, ML-KEM-768 and its hybrid, Ed25519 sign/verify, `chacha20-poly1305@openssh.com`, SHA-256/512, and a caller-supplied `Random` for replays |
| `toyos-ssh`'s `hostkey` on the signing path. `build/signing.rs` reads the owner's image-signing key through `openssh_seed` and writes a minted one through `openssh_private`; both derive the public key from the seed | `toyos_update::sig::public_of`, the crate's since #1019 | nothing |
| `system/sshserver` | `toyos-ssh`, since the SSH track's stage 2 | none directly; it reaches the crate through `toyos-ssh` |
| Image and package signing: `lib/update` (the loader, `/system/bin/update`, `build/signing.rs`, `build/root`, #892's repository keys) | the crate's Ed25519 and SHA-2, since #1019; signing and a seed's public key behind `sign`, on the host | stage 6's bench readings |
| Every SHA-256: the build, `pkg`, `lib/swap`, `system/update`, `lib/update` and the loader | the crate, since #1016: SHA-NI on x86-64, the UEFI loader's in a block that saves the XMM registers it uses; the Armv8 SHA instructions where the target promises them. No slower than `toyos-sha2-hw`, which it replaces, on either bench: on the T14 4.6 times as fast at 64 B and 9 and 6 percent faster at 16 KiB and 1 MiB, on the AMD bench 2.8 times at 64 B, 2 percent at 16 KiB and equal at 1 MiB (#1016's evidence) | nothing |
| ToyOS's own HTTPS clients. On the host: the build's `ureq` (`build/release.rs`) and doom's build script (`apps/doom/build.rs`). In a guest, `osbench internet` (`tests/guest/osbench/src/internet.rs`), on ToyOS and Linux alike; `https_tls13` and `pkg install <url>` are stages 3 and 5 of `issues/the-internet-clients-work-unchanged.md` | all three: the crate's rustls provider through ureq's `rustls-no-provider`, since #1020, its key exchange on X25519 and X25519MLKEM768 alone while the T14 finding stands. Each copies the provider's random source and that group filter | the NIST groups back once the T14 finding is resolved, in all three copies, or once in the crate if it comes to own the policy |
| Wi-Fi's `wlan` (`issues/there-is-no-wifi.md` P6, W2) | nothing yet | PBKDF2-HMAC-SHA1, the 802.11i PRF and RFC 3394 unwrap, which the crate has. AES-CMAC-128, SAE over P-256 by hash-to-element, a public constant-time comparison and a public wiping secret, which it lacks |
| Encrypted DATA storage (`issues/storage-is-layers-and-a-role-is-a-filesystem.md`): the bcachefs engine's `crypt` unlock and its key holder | nothing yet | scrypt (RFC 7914) at bcachefs's default and smallest costs, which the crate has; ChaCha20 at a chosen block counter and raw Poly1305 for the format's own AEAD composition, which it lacks publicly |
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
   - `lib/ssh/Cargo.toml` names no `ring`.
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
   - **Symmetric families: partial, stage 3a**, at 540c55e35, shipped
     build. Stage 3 is not complete. Of 64 symmetric rows, the T14 passes
     53 and the AMD bench 44. No row is slower than the first measurement
     above. Evidence: `ToyOSOrg/evidence` `pr-958`.
     - **The paths that ran.** The kernel's cpu0 records in these boots
       show CR4.OSXSAVE set: XCR0 0xe7 on the T14 and 0x7 on the AMD bench,
       saved by XSAVES and XSAVEC. Each build's ISA line and the crate's own
       CPUID and XGETBV admission agree with them.
       - **The T14:** AES-GCM up to 224 B on AES-NI's one-batch path, and
         beyond that on 512-bit VAES with VPCLMULQDQ; ChaCha20 on AVX-512VL;
         Poly1305 on AVX2 for 16 blocks or more; SHA-512 with its message
         schedule on AVX-512VL; SHA-256 on SHA-NI.
       - **The AMD bench:** AES-GCM on AES-NI and PCLMULQDQ; ChaCha20 and
         Poly1305 on AVX2; SHA-512 on BMI2; SHA-256 on SHA-NI.
     - **Rows that fail, by measured cause.**
       - **ChaCha20 on AVX2 (the AMD bench): a recorded exception.**
         - Since #1056, seal and open of records past 192 B on AVX2 without
           AVX-512VL are one stitched pass of eight-block batches, the
           Poly1305 key in the first; shorter records take the short path. A refusal zeroes the record, as AES-GCM's does since #1054.
           The one-pass open is an owner decision of 2026-10-23.
         - Rows, from the guest table at 035c36d30: ChaCha20-Poly1305 at
           1 KiB and 16 KiB, seal 1.25 and 1.11, open 1.19 and 1.09, reject
           1.14 and 1.17. Reject costs an open because the open is one pass.
         - Linux probes put the 64 KiB rows at 1.04 to 1.10, and the spread is
           per build. 2c00ebbdc and 035c36d30 read 1.04 and 1.10 with the
           same 4,503-byte vector loop at a different alignment and ring
           unchanged. Code placement is the unverified suspect.
         - Exit: an AVX2 ChaCha20 schedule within ring's time, and then the
           rows pass. Until then the rows stay recorded here, as ruled.
       - **ChaCha20-Poly1305 per-message cost.**
         - Rows:
           - 0 B and 64 B (seal, open and reject): the AMD bench 1.04–1.38
             (Linux probe), the T14 0.83–1.25 (guest table);
           - the T14's 1 KiB rows, 1.04–1.05 (guest table).
         - A seal sweep over 1–192 B put ours above ring by a constant per
           message, with a per-block cost that matches ring's.
         - At 540c55e35 the constant is 23–51 ns on the AMD bench and 17–25
           ns on the T14. At 1 KiB on the T14 it is about 37 ns, with
           Poly1305 alone (the reject row) at 377 ns.
         - The candidates are Poly1305's key setup, its powers up to r^8 and
           its finish, erasing the keystream buffer, and the scalar block 0.
           Their shares are not measured. The short path no longer fills or
           erases state buffers; removing them left the T14's 1 B seal at
           1.10 (1.107 to 1.097).
         - The short path's step at 80 B is gone on the T14: with three or
           four blocks in one AVX-512 register set, seal at 80 B fell from
           1.27 to 1.12 against ring. On the AMD bench, AVX2 only, building
           the rows from the key moved the 64 B rows against a same-day
           boot of main: seal 1.34 to 1.24, open 1.26 to 1.17; 0 B and the
           bulk rows are unchanged within noise. Its step at 80 B is
           unmeasured: the sweep has not run there since.
         - Exit: the rows pass on both benches.
       - **One ChaCha20 block on the AMD bench (`chacha20-poly1305-openssh/length`),
         1.087.**
         - Its own unasserted column reads 0.990.
         - In the shipped binary `chacha::legacy_block` stays a separate
           call behind a debug precondition check (`panic_nounwind_fmt`).
           The unasserted build inlines it.
         - Exit: the single-block path is written so the shipped build
           inlines it, as the hot loops were, and the row passes. The T14
           passes it at 0.82.
       - **AES-GCM bulk on AES-NI (the AMD bench).**
         - Rows: 1 KiB and 16 KiB, seal, open and reject, 1.007–1.131 in
           `cryptocase` at #1054's 43690c542. The reject rows newly fail
           there (0.53 → 1.02–1.03): since #1054 a refusal decrypts and
           wipes, so reject costs what open does.
         - With #1054's 16-byte volatile wipe at the refusal, reject at
           16 KiB reads 1.135 and 1.112 (3916 and 4213 ns), against 1.03
           with the `memset`. The wipe's stores cost more than a long
           `rep stosb`; they replace it because a short one costs more
           still (`issues/a-short-memset-on-x86-64-costs-a-rep-stosb-start-up.md`).
           A refusal is an error path, so the trade stands until that issue's
           exit returns the refusal to a fill.
         - The stitched eight-block seal is about 277 instructions per
           128 B, so it is bound by instruction count, not spills.
         - Before #1054, when reject was GHASH alone: CTR (open less
           reject) was about 1.95 µs per 16 KiB for AES-128 and 3.0 µs for
           AES-256, and GHASH alone passed at about 0.5.
         - A VEX-encoded build of AES-NI measured slower and is not to be
           retried. It kept all fifteen AES-256 round keys in registers and
           serialised the eight blocks (CTR 2.0 to 2.2 µs).
         - Open is one pass since #1054 (the seal stitch, hashing each
           batch's input). In `cryptocase` at #1054's 43690c542, open at
           16 KiB reads 1.03 for both key sizes (AES-256 was 1.26 on the
           bench's Linux before), seal 1.13 and 1.03, and reject, which now
           decrypts and wipes, 1.03.
         - Exit: a schedule with fewer instructions per block, in counter
           generation and the Karatsuba folds, and the rows pass.
       - **AES-GCM bulk on 512-bit VAES (the T14).**
         - Since #1054: 16-block steps, three to a reduction with H^48..H^1
           held in the key, schoolbook products, counters in a register,
           round keys loaded per step, and a one-pass open.
         - On the bench's Linux (`aes-gcm` probe, shipped profile), 16 KiB
           and 64 KiB went from 1.00–1.08 against ring and 1.40–1.41 against
           AWS-LC to 0.74–0.77 and 0.96–1.02. Taking each technique back out
           measured: scalar counters 1.10–1.65 against AWS-LC, a reduction
           per 16 blocks 0.99–1.19, a two-pass open 1.11 on open; Karatsuba
           in place of schoolbook made no difference.
         - In `cryptocase` at 43690c542, every row at 1 KiB and 16 KiB
           passes: seal 0.91–0.94 and 0.77–0.78, open 0.76–0.86, reject
           0.76–0.88. Key setup reads 0.78–0.81.
         - The 0 B reject rows failed there (1.12). The refusal's wipe was a
           `memset`, which costs a `rep stosb` start-up on ToyOS
           (`issues/a-short-memset-on-x86-64-costs-a-rep-stosb-start-up.md`).
           With #1054's 16-byte wipe they read 56.7 and 61.2 ns (0.59,
           0.63), main's 57–61, and pass.
         - The same wipe costs at 16 KiB: reject reads 1898 and 2094 ns
           (0.988, 0.982, still passing), against 0.76–0.77 with the
           `memset`. The trade and its exit are the AMD entry's above.
         - Exit: the guest rows pass.
         - Evidence:
           https://github.com/ToyOSOrg/evidence/tree/db8683ad53396c00cef576007349d3662d4c7d78/r3
           and `r4` beside it.
       - **SHA-512 at 16 KiB on BMI2 (the AMD bench), 1.035.**
         - A vector message schedule without AVX-512 measured 1.385 and was
           removed.
         - Exit: a scalar schedule within ring's time.
       - **OpenSSH key setup: a recorded exception.**
         - Rows: 2.5 on the AMD bench, 2.77 on the T14.
         - `SealingKey::new` is one 64-byte copy. Dropping the key erases it
           with four 16-byte volatile stores and a fence. Ring's 1.6–1.9 ns
           is about the copy alone.
         - The erasure is a stage-4 property and stays.
         - Exit: none at this stage. The row is measured and recorded here,
           and stage 4's erasure contract decides whether the key keeps it.
     - **256-bit VAES GCM is out.**
       - It is GCM's path on CPUs with VAES and VPCLMULQDQ but without
         AVX-512: Zen 3, and Alder Lake through Arrow Lake.
       - No bench has that ISA, and TCG refuses VPCLMULQDQ. So, by this
         track's rule for paths no bench can run, those CPUs take AES-NI.
       - Exit: a bench with that ISA, on which the path is written and
         measured.
     - **Correctness of the paths.**
       - Each build first checks ours against ring through the public API
         at every length from 0 to 1153 B. This covers AES-GCM and
         ChaCha20-Poly1305 seal, open and rejection with AAD of 0, 5, 16, 17
         and 33 B, the OpenSSH construction, SHA-2 and HMAC. The AEADs also
         run at 1281, 1535, 4113 and 16401 B, past the 512-bit path's
         reduction groups and its counter's first low-byte carry. It runs in
         the `cryptocase` boot and the QEMU row.
       - The host tests run on the portable code and, under Rosetta, on
         AES-NI and PCLMULQDQ. No host here runs the 512-bit path: the
         bench's own checks are its correctness evidence.
       - Vector counter carries past 2^32 blocks are out of ring's reach;
         the 64-bit `wrapping_add` settles them by inspection.
     - **Constant time, by inspection.**
       - The only static table the instruction paths index is SHA-2's
         round constants, by round.
       - Their branches test only public lengths, round counts and CPU
         features.
       - The fixed-versus-random harness is stage 4's own PR.
     - **A cause not to rediscover: the shipped profile in vector code.**
       - In `[profile.toyos]`, `array::map`, `zip` and slice iterators
         stayed out of line inside vector loops. Each call spilled every
         live register, and AES-GCM ran 4–5× its unasserted build on the
         T14.
       - A large array spilled or moved is a `memcpy`, which is a bare
         `rep movsb` on ToyOS. The 240-byte AES-256 key schedule cost
         25–38 ns per call that way.
       - Hot loops therefore index fixed-size arrays and read round keys in
         place. The evidence holds the two binaries' call lists.
   - **First measurement, not a verdict.** `toyos-ssh`'s fuzz
     (`lib/ssh/tests/fuzz.rs`), on the crate since stage 1: 33.92 s and
     15.46 s in two runs on the development Mac (an M4 Pro). On ring it took
     1.27 s in main's nightly host job, on GitHub's runner.
4. **Secure.** **Exit**, for each primitive a consumer uses:
   - **Typed domains.** The private `V<N>` is replaced by distinct types:
     integers, Montgomery residues per modulus, field elements and scalars per
     curve. Passing one where another is meant does not compile. A secret
     owner's in-place initialisation is enforced by its type rather than by
     call order. Debug assertions remain only for the limb bounds types cannot
     express.
   - **Arithmetic held against integers.** The owner rejected fiat-crypto
     (2026-10-21): ToyOS keeps its own crypto stack, with no fiat code as a
     dependency of any kind, and ring and RustCrypto only as test-only
     oracles. The crate's dependencies of every kind name none since #993.
     `Cargo.lock` still does:
     - `curve25519-dalek` 4.1.3 names `fiat-crypto` 0.2.9 behind
       `cfg(curve25519_dalek_backend = "fiat")`, which nothing in the tree
       sets, so no build compiles it. It is reached through `toyos-crypto`'s
       own oracles `x25519-dalek` 2.0.1 and `ed25519-dalek` 2.2.0, and
       `lib/update`'s `ed25519-dalek` oracle: test-only since #1019.

     `num-bigint`, the crate's test-only integer oracle, holds:
     - `field25519` and `scalar25519`;
     - P-256/P-384's field and scalar arithmetic;
     - Poly1305.

     It holds them on every pair of limb and modulus boundary values, on
     seeded values, on chains that feed each result back unreduced, on
     inversion, and on distributivity and subtraction (`test_arithmetic`).
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
     `lib/ssh/tests/fuzz.rs` does.
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
       the routine or the compiler changes. Its metal rows,
       `crypto_secret_timing` and `crypto_secret_timing_curves`, are
       named-only: a metal pass takes them by their whole name or their boot,
       never unfiltered.
   - **Negative controls.** Each check a forgery would pass is mutated as a
       checked patch, and its exact test fails: the tag compare, point and
       range validation, S < L, and the lengths. No mutation runner is
       committed.
   - **`unsafe`.** Miri runs the `portable` configuration's unit tests on the
     host, covering the volatile wipes and the `ct` barriers. The run is
     recorded as evidence.
   - **Audit.** Whether a human cryptographer audits the crate is the owner's
     decision, and it is open.
   - **Status: the test-side items, #993.** They hold whatever
     arithmetic sits underneath, so the speed work's rewrite is held to them.
     - **Done:**
       - The vectors: 43 Wycheproof files, every case judged or counted
         outside the interface by a named reason and held to the file's
         `numberOfTests`; CAVP's SHAVS (SHA-256/384/512), GCMVS encryption
         (AES-128/256) and ECDSA SigVer and PKV, and ACVP's HMAC and ML-KEM,
         each held to a count taken from its text apart from the parse
         (`lib/crypto/tests/wycheproof.rs`, `nist.rs`).
       - The integer oracle above, which replaced fiat-crypto in Poly1305's
         test, and ML-KEM's exhaustive reduction and compression.
       - The differential (`tests/differential.rs`) and the fuzz
         (`tests/fuzz.rs`), over every CPU. A primitive with no instruction
         path runs in the `portable` form alone. On the development Mac the
         crate's three forms took 20 s of wall time and 15 s of CPU on main,
         and take 74 s and 396 s with this, nearly all of it ours and
         RustCrypto's curve arithmetic in the `portable` form.
       - Negative controls: 15 checked patches, each red by its named test,
         two only after a test was added for it.
       - Miri over every `portable` unit test but the provider's (ring is
         C), with `MIRIFLAGS=-Zmiri-disable-isolation` since #1016 moved
         NIST's ML-KEM keygen vectors to `tests/acvp/`, whose file the test
         reads: 25 pass and ML-KEM's exhaustive reduction is ignored under it;
         the heavy tests take a spread of their inputs there. A patch that
         writes past `erase`'s array is caught.
       - Evidence: `ToyOSOrg/evidence` `pr-993`,
         https://github.com/ToyOSOrg/evidence/tree/187abe7793116cbbbfb4dec63098032b1b8d61de/pr-993/r2
         and the first round's beside it.
     - **Constant time, first verdicts** at 7a36687d0 (#993's base
       b9e4c5862, before #990 rewrote the NIST arithmetic), 1,000,000
       measurements a routine, the pinned `rust` at a56b4057c. The harness
       (`tests/guest/rust-tests/src/constant_time.rs`) runs a null and a
       leaky control at the start and end of each program, and they held on
       every boot: null |t| 0.9 to 2.2, leaky 74 to 1,311.
       - The fast program passes every routine on the T14 (Intel signature
         0x806c1) and the AMD bench (0x860f01).
       - The curves program passes every routine on the AMD bench.
       - **Finding, the T14, at 7a36687d0 (resolved since):** P-256 public
         key LEAKED (max |t| 73.1, raw t 66.9) and P-256 agreement LEAKED
         (max |t| 83.4, raw t 76.0); the fixed scalar was about 510 ticks
         slower of about 1.5 M. P-384 agreement LEAKED (max |t| 5.79, raw
         t 5.77). P-384 public key and Ed25519 passed. Its fixed scalar 1
         held the running total at the identity for 51 of 52 windows.
       - **The T14 at #1036's 7c860ab7b** (#990's NIST arithmetic, #1000's
         Booth windows, #1013's comb, odd representatives and field, and
         #1036's BMI2/ADX field kernels), DOITM off, signature 0x806c1,
         microcode 0xbe, 1,000,000 measurements a routine: every curve
         routine passes. P-256 public key max |t| 1.72 and agreement 2.78;
         P-384 public key 2.79 and agreement 1.49; Ed25519 1.70. The
         controls held: null 2.57 and 1.79, leaky 455.97 and 1,005.05. The
         finding does not reproduce on current code. Which of those changes
         removed it is not measured. The AMD bench has not run the curves program on
         current code. Evidence:
         https://github.com/ToyOSOrg/evidence/tree/6993f41cdaa886860b4d40bf766ac092cbe4d14f/pr-1036/r3
       - **The T14 at #1046's 6ea592362** (2^255 - 19 in four words with
         BMI2/ADX kernels on x86-64), signature 0x806c1, microcode 0xbe,
         1,000,000 measurements a routine: X25519 agreement passes with max
         |t| 2.30, X25519 public key 1.83, Ed25519 derivation and signature
         2.58. The controls held: null 1.25 to 2.29, leaky 1,258 to 1,309.
         Evidence:
         https://github.com/ToyOSOrg/evidence/tree/84fd34d3e1ad6f631101c8c559bdc2511e7d0677/pr-1046/r3
         The AMD bench at 23840e4ed (signature 0x860f01, microcode
         0x860010d): X25519 agreement passes with max |t| 3.32 and public key
         2.65; null 1.82 and 1.04, leaky 210.6 and 194.7. Its Ed25519 run is
         pending. Evidence:
         https://github.com/ToyOSOrg/evidence/tree/6488b4fbb6785e65332d03fb504fe3849d89810a/pr-1046/r4
       - **#1063's P-256 register kernels at 9a5d4fad5** (with the AVX2
         table read), 1,000,000 measurements a routine, no CPU timing mode:
         - The T14 (0x806c1, microcode 0xbe): P-256 public key max |t|
           3.20 and agreement 1.51; P-384 public key 1.19 and agreement
           1.42; Ed25519 2.73. Null 1.22 and 1.70, leaky 855.8 and 890.6.
         - The AMD bench (0x860f01, 0x860010d): P-256 2.99 and 1.49; P-384
           2.47 and 2.43; Ed25519 1.66. Null 1.14 and 2.12, leaky 140.1
           and 169.3.
         - Evidence:
           https://github.com/ToyOSOrg/evidence/tree/aa3369406af00b46a4b76476ef4cea7df040e30f/combined/r3
       - Their judge asks only that each job ran and its controls held; the
         verdicts are this record's. The first boots, of the fast program at
         ec5a0cd18, read LEAKS everywhere: that harness made each random
         input next to the clock, so those readings are no finding.
       - Evidence: https://github.com/ToyOSOrg/evidence/tree/18e8243a9e9fb4ec4edaeec68ace23abe98c16b9/pr-993/r3
     - **AES-GCM at #1054's 43690c542**, 1,000,000 measurements a routine,
       on the T14 (signature 0x806c1) and the AMD bench (0x860f01).
       - Routines: AES-128 and AES-256, key setup and seal and key setup
         and refusal, at 64 B and, new, 1 KiB, which takes the stitched
         loops and the refusal's wipe.
       - Every routine passes: max |t| 1.07 to 3.27 on the T14 and 0.61 to
         1.79 on the AMD bench.
       - The controls held: null 1.68–1.69 and 0.83–1.17, leaky 1,119.7 and 1,122.5
         and 109–190.
       - The job took about 465 s and 434 s.
       - With #1054's 16-byte refusal wipe in place of the `memset`, every
         routine passes again: max |t| 1.10–2.60 on the T14 and 1.00–2.49 on
         the AMD bench.
       - Evidence:
         https://github.com/ToyOSOrg/evidence/tree/db8683ad53396c00cef576007349d3662d4c7d78/r3
         and `r4`.
     - **Not timed:** ML-KEM key generation. Its matrix expansion rejects
       samples by rho, a public output of the secret seed, so no two classes
       can share it. Exit: a measurement with rho held fixed, which needs an
       entry below the seed (a proposal; no API takes rho today).
     - **Not timed, by design:** scrypt's ROMix. Its second loop reads the
       table entry the secret state names (RFC 7914 §5); that access pattern
       is what makes it memory-hard. Its PBKDF2 and Salsa20/8 are timed.
     - **PBKDF2 and Salsa20/8**, at #1029's ba90ee11c on the T14 (Intel
       signature 0x806c1, microcode 0xbe), 1,000,000 measurements each:
       PBKDF2-HMAC-SHA-256 (32 B secret password and salt, 2 iterations,
       64 B) passes with max |t| 1.35, and the Salsa20/8 core with max
       |t| 1.99. The controls held (null 1.51 and 1.56, leaky 100.9 and
       1,177.7). Not yet run on the AMD bench. Evidence:
       https://github.com/ToyOSOrg/evidence/tree/e7a381d2136c205e32a6812845abde6ce7fd5fd1/pr-1029/r2
     - **Open:** typed domains (the speed work's); each secret routine's
       module contract naming its secret inputs; the code inspection; the
       audit.
5. **Consumers switch.** Rules for every consumer:
   - A consumer on ring today switches only after stages 3 and 4 hold for
     every primitive it uses.
   - A consumer that ships no crypto today switches after stage 4 holds for
     its primitives. Parity for those primitives is a later stage, not a
     gate.
   - Each consumer's old dependency is deleted in the same landing.
   - NIST's ACVP vectors are under `NIST-Software`, which the licence
     allowlist (`build/licence.rs`) does not name. They are test input in
     `tests/acvp/`, outside the crate's directory, so no image ships them.

   The consumers:
   - `toyos-ssh`'s protocol offers `mlkem768x25519-sha256` first, on the
     crate's ML-KEM-768 and its SSH combiner. sshserver ships it since stage
     2 of the SSH track, before this track's stages 3 and 4 hold for SSH's
     primitives, by the owner's ruling (2026-10-22): "Use our own crypto where
     possible; we optimize and improve until done, but we can already use
     it."
   - Every SHA-2 is the crate's since #1016 (owner, 2026-10-22: "use our
     own crypto where possible"): `build/`'s digests, `pkg`, `lib/swap`,
     `sdk/icon`'s test, `lib/update`, the loader and `system/update`.
     `toyos-sha2` and `toyos-sha2-hw` are deleted; the crate's SHAVS tests
     replace theirs. NIST's ACVP vectors moved to `tests/acvp/`, outside the
     crate's directory, once the crate entered an image.
   - ToyOS's own HTTPS clients use the crate's rustls provider. The owner
     ruled on 2026-10-13 that the crypto goal overrides his ruling of
     2026-10-02 in `issues/the-internet-clients-work-unchanged.md` stage 3,
     and on 2026-10-22 that the tree uses the crate now. The host's two, the
     build's agent and doom's build script, take it since #1020, on X25519
     groups alone while the T14 finding stands. `ToyOSOrg/ring` stays as a
     fork so that unmodified programs build.
     - The `https_tls13` metal row passes on it.
     - The `cc` row of
       `issues/the-build-runs-host-tools-outside-rust-and-qemu.md` names ring
       only as the crate's and `lib/update`'s dev-dependency oracle, whose C
       and assembly every `--ci host` run compiles. Done (#1020).
   - Wi-Fi's P6 is this crate's AES-CMAC, SAE by hash-to-element, public
     constant-time comparison and public wiping secret, each against its RFC
     or Annex J vectors.
6. **The root of trust.** The loader and `lib/update` verify, and the host
   signs, with the crate. The owner ruled on 2026-10-13 that this is the
   track's last stage, taken only after every stage 4 exit holds, and on
   2026-10-22 that the tree uses the crate now and improves it in place
   ("use our own crypto where possible"); #1019 takes it.
   **Exit**:
   - The verifier's rules are RFC 8032's and ToyOS's security
     requirements, stated at `PublicKey::from_bytes` and
     `PublicKey::verify` (`lib/crypto/src/ed25519.rs`). ring and
     `ed25519-dalek` 2.2's `verify_strict` are host cross-checks, not the
     definition of correctness; the owner ruled that canonical-encoding
     rejection stays and verification is not weakened to match a library
     (#1044). Done:
     - A key decodes under section 5.1.3 (y below p, an x for it, x = 0
       only with the sign bit clear) and is not one of the eight points of
       order dividing 8. Mixed-order keys are taken.
     - A signature is 64 bytes, R || S, with S below L (section 5.1.7). It
       is accepted when [S]B - [k mod L]A encodes to R's bytes, for
       k = SHA-512(R || A || M) over the bytes received; that also makes R
       decode canonically. Section 6's reference `verify` reduces k, as
       ring and `verify_strict` do; section 5.1.7's literal equation does
       not, and the two differ only for a mixed-order A (speccheck case 3
       fails the unreduced one).
     - One refusal goes beyond RFC 8032. Anyone can sign for a small-order
       key: R = [S]B verifies under it whenever 8 divides k.
     - An R of order dividing 8 is taken where the equation holds, as RFC
       8032 takes it. The owner accepted removing #1044's extra refusal on
       the condition, argued in #1047, that it adds no security property
       the contract requires. Under a prime-order key such an R is the
       identity with S = k a, which only the key's holder can make, and
       RFC 8032 signing makes it whenever r is a multiple of L.
     - Times 8, the reduced-k equation is the cofactored one, which accepts
       more where R or A has a nonzero torsion component (cases 4 and 5);
       RFC 8032's key generation and signing make neither.
   - The edge corpus is Wycheproof's `ed25519` and the twelve cases of
     Chalkias et al., "Taming the many EdDSAs" (2020). Every Wycheproof
     verdict is the file's. `lib/crypto/tests/ed25519_verdicts.rs` gives
     each speccheck case and each known disagreement an expected verdict
     and the rule deciding it, with both cross-checks' verdicts pinned.
     Done (#1044, #1047):
     - **`verify_strict` differs on a small-order R and one class of
       key.** It refuses every R of order dividing 8: R = O with S = k a
       under an RFC 8032 key, and speccheck case 2 (a mixed-order key and
       a small-order R), both of which satisfy the equation. Of the 38
       encodings with y of p or more, it decodes the 20 whose y - p is on
       the curve and not small-order (y - p in 3-6, 9, 10, 14-16 and 18,
       either sign), which section 5.1.3 refuses. Signing for one takes
       the discrete log of a point chosen by its y, so no signature verdict
       is known to differ there.
     - **ring differs on small-order keys and non-canonical small-order
       keys.** It accepts a signature satisfying the cofactorless equation
       under each of the 14 encodings of a small-order point, including the
       6 non-canonical ones (y of p or p + 1, and x = 0 signed). Whether it
       decodes the 20 mixed-order keys with y of p or more is not known: no
       signature can show it. In the speccheck cases that is 0 and 1
       (small-order key) and 11 (the key (0, -1) with x = 0 signed).
     - All three refuse cases 4 and 5 (only the cofactored equation
       holds), 6 and 7 (S of L or more), 8 and 9 (a non-canonical R) and
       10 (a non-canonical key under which ring's equation fails too), and
       accept case 3, a mixed-order key and R satisfying the reduced-k and
       cofactored equations. ToyOS and ring accept case 2 and R = O under
       an RFC 8032 key; all three refuse the seven other small-order R under
       one, which no S can make satisfy the equation.
   - The loader verifies a signed image on the T14 and the AMD bench, with
     its verification time measured on each. Done (#1019's probe image,
     #1016's evidence): 253,975 counter ticks on the T14 at its stated
     2.4192 GHz, about 105 µs, and 122,724 on the AMD bench, whose CPU states
     no counter rate.
   - An AArch64 QEMU loader boot verifies a signed image. Done:
     `a_bent_supervisor_is_refused_and_the_other_slot_boots` boots QEMU's
     AArch64 machine too.
   - `ed25519-dalek` and the loader's `sha2` leave `Cargo.lock`'s non-dev
     packages. Done (#1019).

## Weaknesses until the exits hold

- On x86-64 nothing but the code keeps secret work's timing independent of
  its data. The T14's store-forwarding predictor once timed P-256 by its
  data, which DOITM, PSFD or SSBD removed
  (`issues/intels-store-forwarding-predictor-times-constant-time-code-by-its-data.md`).
  Intel's MXCSR-dependent timing can delay the AVX2 Poly1305's `VPMULUDQ`
  on its data
  (`issues/the-avx2-poly1305-runs-under-intels-mxcsr-dependent-timing.md`).
  **Owner**: this track. **Exit**: both issues'.
- Erasure is what `lib/crypto/src/secret.rs`'s contract states: secret
  owners, each key agreement's shared value and named workspaces are
  erased. Since the parity work's 5x51 field (#992), 2^255 - 19 field
  elements, NIST residues and curve points are not erased one by one. The
  owner approved that on condition of a measured benefit (M1: 0.80–0.94 of
  per-temporary erasure on the T14, 0.79–0.88 on the AMD bench) and a
  reviewed contract. Compiler
  copies, registers, spills and caller-held bytes are not wiped.
- The host's HTTPS clients exchange keys on X25519 and X25519MLKEM768
  alone, and a server offering only P-256 or P-384 is refused. Each client
  keeps the two groups itself (`build/release.rs`, `apps/doom/build.rs`).
  The reason, stage 4's T14 finding that P-256 key derivation and agreement
  and P-384 agreement leaked, no longer holds: the T14 passes all four at
  #1036's 7c860ab7b. **Owner**: this track. **Exit**: the clients take the
  provider's groups whole.
- ToyOS's AArch64 targets hash with the portable SHA-2 rounds: no target
  promises the Armv8 SHA instructions, and neither the kernel nor the crate
  detects them at run time. **Owner**: this track. **Exit**: the crate takes
  them where a run-time word from the kernel says the CPU has them, measured
  on an ARM bench.
- scrypt (`lib/crypto/src/scrypt.rs`) runs portable Salsa20/8 and BlockMix
  on every target, with no vector path, and its speed is unmeasured on
  either bench and against any other implementation. A slow derivation
  costs only unlock time at bcachefs's default cost (N = 2^14, r = 8,
  p = 16), but it also lowers the work factor a key holder can afford for
  the same wait. **Owner**: this track. **Exit**: a bench measurement
  against a reference implementation at that cost, and a vector path if
  the measurement calls for one.
- **Owner**: the orchestrator. **Exit**: stage 4 states what the shipped
  targets keep, and a consumer holding a long-lived key stores it under a
  wiping owner or a Key Locker handle.
