---
status: open
kind: tooling
opened: 2026-10-02
---

# The guest test crate keeps the cut HTTPS test's dependencies

`tests/toyos-rust-tests/Cargo.toml` names `ureq`, `rustls`,
`rustls-rustcrypto`, `rustls-pki-types`, `webpki-roots` and `sha2`, which
`7c47e8930` added for `tests/toyos-rust-tests/src/bin/https_fetch.rs`.
`520c0d129` deleted that program, and `git grep` for the six under
`tests/toyos-rust-tests` finds the manifest and its lockfile alone. The program
comes back with stage 3 of
`issues/design-debt/the-internet-clients-work-unchanged.md`, on another
provider.

**Exit**: the six lines are gone, the lockfile follows, and
`cargo test --test toyos-build` is green.

Owner: the orchestrator.
