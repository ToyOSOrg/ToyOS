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
3. **TLS.** `rustls` with the `graviola` provider and the Mozilla root set
   as a pinned data file in the signed image, used as published; what breaks
   is fixed in this repository's layers or carried upstream as a fork.
   **Exit**: an unmodified `rustls` client completes a handshake with a
   host-side server in the harness, and refuses a wrong name and an untrusted
   root.
4. **HTTP.** An HTTP crate used as published. **Exit**: an unmodified client
   fetches a body over HTTPS, following a redirect, byte-exact.
5. **The proof is `pkg install <url>`** — the package track's HTTPS stage
   (`issues/filesystem/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`).
   **Exit**: gbae installs from its GitHub release in QEMU against a
   harness-run server, then once from GitHub itself with the owner watching,
   and on the T14 over the real network.
