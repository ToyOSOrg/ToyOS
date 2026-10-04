---
status: open
kind: track
opened: 2026-09-26
---

# The internet clients work, unchanged

Owner goal: the internet clients work perfectly. Under "existing Rust just
works" that means a program written against `std::net`, `rustls` and an HTTP
crate such as `ureq` builds for ToyOS unchanged and behaves as it does on any
other OS. TLS and HTTP are not written here: the crates are used as published.
What this track owns is the layer under them that only ToyOS can supply —
netd, `toyos::net` and std's ToyOS networking in the `rust/` fork.

## Stages

1. **Names resolve.** netd's own resolver behind `std::net::ToSocketAddrs`.
   Landed (#511). Whether its wire decoding should be `hickory-proto` rather
   than `toyos-dns`'s own reader is
   `issues/design-debt/toyos-dns-decodes-what-a-published-crate-decodes.md`.
2. **TCP holds up.** Bounded connect, resets surfaced, half-close, large and
   lossy transfers byte-exact, timeouts honoured, a departed client freeing
   everything in netd. **Exit**: each is a guest test against the host's own
   TCP stack, with a hash over every byte moved.
3. **TLS.** `rustls` with the `ring` provider (owner, 2026-10-02; `graviola`
   is an option for later) and the Mozilla root set as a pinned data file in
   the signed image, used as published; what breaks is fixed in this
   repository's layers or carried upstream as a fork. This stage owns the
   T14's `https_tls13` row, and stage H of
   `issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`
   points here for it. The guest test of that name ran the tree's one ToyOS
   program on `rustls`; the program and its judge's server installed
   `rustls-rustcrypto` 0.0.2-alpha, and #660 cut both. The row comes back on
   `ring` and waits for it; `rustls-rustcrypto` does not come back (owner,
   2026-10-02). Published `ring` 0.17.14 does not link for
   `x86_64-unknown-toyos`: its `build.rs` picks its assembly by an OS list
   that lacks `toyos`, and `src/rand.rs` has an OS list of its own. With
   `"toyos"` added to `build.rs`'s `LINUX_ABI` and `target_os = "toyos"` to
   `src/rand.rs` it builds for both ToyOS targets. In an x86-64 QEMU guest
   it passes known answers (SHA-256,
   SHA-512, HMAC, X25519, ChaCha20-Poly1305, AES-GCM, Ed25519, P-256,
   `SystemRandom`), and `ureq` 3.4.2 on `rustls` 0.23.45 fetches 320000
   bytes over TLS 1.3 from a server on the host and refuses a wrong name and
   an untrusted root (#682, comment 5968053596). What stands before it
   lands: a git fork's
   `build.rs` runs `perl`, an arrival
   `issues/build/the-build-runs-host-tools-outside-rust-and-qemu.md` does not
   declare, and leaves C asserts on, so `__assert_fail` is undefined unless
   ring builds with `debug = false` or `toyos_c` is linked; `src/build.rs`
   gives the C compiler's environment (`cc_env`) to userland builds alone, so
   a test crate's C is compiled by the host's `cc`; and not run: AArch64, the
   T14, a `git =` dependency, the licence gate over ring in an image.
   `rustls-rustcrypto` is
   still named by doom's build script, which installs it on the host.
   Open: what doom's build script installs instead.
   **Exit**: `https_tls13` is a `METAL` row: on the T14's I219 an unmodified
   `ureq` and `rustls` client on the `ring` provider fetches over TLS 1.3
   from a server the harness runs, and refuses a wrong name and an untrusted
   root; and no manifest names `rustls-rustcrypto`.
4. **HTTP.** An HTTP crate used as published. **Exit**: an unmodified client
   fetches a body over HTTPS, following a redirect, byte-exact.
5. **The proof is `pkg install <url>`** — the package track's HTTPS stage
   (`issues/filesystem/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`).
   **Exit**: gbae installs from its GitHub release in QEMU against a
   harness-run server, then once from GitHub itself with the owner watching,
   and on the T14 over the real network.
