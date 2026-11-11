---
status: open
kind: tooling
opened: 2026-11-11
---

# No test holds what a full DATA volume takes with no commit between

A bucket is written again only once its emptying is durable
(`lib/bcachefs/src/engine/mount/allocator.rs`), so on a volume its files
filled, the space an unlink frees is not free at once. `Mount::grant` does
not refuse a change for room while an emptying awaits durability (`settled`).

The engine's tests of a full volume commit between the two:
`a_full_data_volume_can_release_a_large_orphan_and_use_its_capacity_again`
(`lib/bcachefs/src/engine/tests/data.rs`) and
`a_full_volume_refuses_changes_by_name_and_still_removes_names_and_unmounts_clean`
(`lib/bcachefs/src/engine/tests/buckets.rs`). A scratch copy of the first
without its second commit, at 422915e2c, refused a 1 MiB write before the
unlink (`NoSpace`) and took it after the unlink and release. It is not
committed.

The second removes 21 names between two commits on a full volume, not every
file, and a removal's grant can still answer `NoSpace("no room left for the
btree nodes a removal may write")` (`Draw::Freeing` in `Mount::grant`). No
test removes every file of a full volume with no commit between.

## Exit condition

On a volume its files filled, host tests hold that an unlink and release
followed by a write of what it freed is accepted with no commit between them,
and so is a delete of every file on it. Each is red when the grant refuses
instead of waiting.
