---
status: open
kind: tooling
opened: 2026-09-26
---

# A guest lane leaves the toolchain tarball in its `$TMPDIR`

`src/ci.rs`'s guest lane points `$TMPDIR` at a `TempDir` and ends on
`nothing left in $TMPDIR`. Its `the toolchain` step is `release::install`, which
downloads `toyos-toolchain.tar.zst` to `std::env::temp_dir()`, unpacks it,
and never removes it. So every guest, tcg and audio job reds on
`left … : toyos-toolchain.tar.zst` whatever its suite did. Seen on all fifteen
of those jobs in the nightly on PR #524's branch (run 36266825579). Neither
side is that branch's: both are main's, the step from `ba68664b` (#529), the
install from before it. Main has had no nightly since #529 landed.

**Exit**: the tarball goes into a `TempDir`, or is removed once it is
unpacked, and a guest lane's last step is green on a nightly.
