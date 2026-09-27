---
status: open
kind: tooling
opened: 2026-09-27
---

# Netstack3 needs a compiler as new as Fuchsia's, and the host needs `RUSTC_BOOTSTRAP` for it

The mirror (ToyOSOrg/netstack3, `forks.toml`) is written for Rust's
`main` as Fuchsia's toolchain builds it, and the fork (`rust/`, 1.99.0-dev at
the pin `fbf6ad143d8`) is behind it on two counts, each paid with a flag
`userland/Cargo.toml` gives the mirrored crates alone through
`profile-rustflags`:

- **`!` as a type.** Fuchsia converted `Infallible` to `!` in the core on
  2026-09-22; `never_type` is stable on Rust `main` at 1.101 and unstable in
  the fork. Thirteen crates get `-Zcrate-attr=feature(never_type)`.
- **Polonius.** `netstack3-device`'s `gro.rs` borrows only Polonius accepts;
  nothing in Fuchsia's GN passes a flag, so its compiler presumably defaults to
  it. That crate gets `-Zpolonius=next`.
- **Lints**: every mirrored crate gets `--cap-lints=allow`, since a warning
  in upstream's code is upstream's to fix.

`profile-rustflags` is a cargo feature and the two `-Z` flags are unstable, so
every stable cargo that reads `userland/Cargo.toml` runs with
`RUSTC_BOOTSTRAP=1` (`userlandhost::CARGO_ENV`): the host job's userland
tests, the licence gate's metadata and the build's `cargo clean`. The fork's
own cargo and rustc, which build every guest, need nothing. The same variable
lets any userland crate of ours write `#![feature(...)]` in the host job
unnoticed, as the fork's dev-channel compiler already lets it in every guest
build; nothing refuses one outside the mirror.

**The cost of keeping it**: Fuchsia moved the core and its libraries 317
times in the year to 2026-09-26 (research at the spike), with no releases and
no semver, and adopts nightly behaviour within days (`!` four days before the
pinned commit, GRO the same week). A sync that needs a newer compiler than the
fork carries is a fork merge from Rust `main` first; one that needs a new
unstable flag is a new row in `userland/Cargo.toml`.

**What would close it**: the fork on a compiler where `never_type` is stable
and the mirror builds without `-Zpolonius=next`, the `profile-rustflags` rows
and `userlandhost::CARGO_ENV` deleted with them.
