---
status: open
kind: track
opened: 2026-08-01
---

# The `lib/bcachefs/` crate does not implement bcachefs

The active ROOT/build and DATA paths still use the ToyOS-native format in
`lib/bcachefs/src/{fs,superblock,btree}.rs`. Its `BCFS` and `BTND` magics are not
the genuine bcachefs format. `lib/bcachefs/src/upstream/` now implements a
read-only subset of the real format; it has no journal writer or mutation path
and currently refuses encryption and snapshots. That reader does not make the
existing writable volume format interoperable.

Owner decision, 2026-10-17: retain genuine bcachefs as the default filesystem,
implemented in Rust, with the shared-volume desktop model in
`issues/storage-is-layers-and-a-role-is-a-filesystem.md`. The format comparison
is settled. Independent per-user encryption is deferred; encrypted user storage
remains required for the first release. Implement the genuine single-drive
configuration first, including its required recovery and integrity protections.
This does not commission snapshots, compression or multi-drive implementation.

The crate must grow into the real bcachefs on-disk format. This track owns the
remaining implementation work; the current ToyOS-native writable format does
not satisfy the decision. No replacement custom format or encryption-format
extension is approved.

The loader supplies authenticated bootstrap executables explicitly. The
userland fileserver mounts ROOT; the kernel needs no filesystem reader to
start it. A genuine bcachefs implementation can be checked against upstream
tools, unlike the interim ToyOS-native format.

**How that judge is used, since a judge is not a dependency:** upstream's `bcachefs-tools` is a *development*
instrument, run by a builder on a volume ToyOS wrote, with its `fsck` and its
readback pasted into the pull request. What the suite runs against is committed
fixtures — volumes upstream's tools wrote, recorded in `NOTICE`. Nothing is
fetched and no distribution is depended on.
