---
status: open
kind: defect
opened: 2026-09-26
---

# DATA takes a directory whose parent was never made

`userland/fsd/src/data.rs` keeps DATA's directories on the volume as marker
entries, so a directory survives a reboot. What it does not do is refuse one
whose parent is missing: `DataVolume::require_parent_dir` refuses a parent that
is a file or a symlink and nothing else, because the interim format makes
every prefix of a name a directory. So `create_dir("/home/toy/Apps")` succeeds
with no `/home/toy`, and `std::fs::create_dir_all` makes the leaf and nothing
above it — init makes every level in turn for this reason (`make_dir` in
`userland/init/src/main.rs`).

## Owner

The storage track, `issues/filesystem/storage-is-layers-and-a-role-is-a-filesystem.md`:
real bcachefs under DATA carries directories.

## What would close it

DATA's `mkdir`, a create and a rename refuse a missing parent, with a host test
in `userland/fsd` that asks each for one.
