---
status: open
kind: defect
opened: 2026-09-26
---

# A refused free in `toyos-fat32` keeps the cluster it could not let go, on top of the entry already erased

Read, not measured; found reviewing PR #492
(https://github.com/ToyOSOrg/ToyOS/pull/492#issuecomment-5840276317).
`toyos-fat32/src/fs.rs` has two call sites where a chain-free's own device
write can be refused, and each already has an earlier step that cannot be
undone once it lands:

- `rollback_to` (`fs.rs:747`) discards `shrink_chain`'s result outright:
  `let _ = self.shrink_chain(f, size);`. It runs after a failed growth, to
  put the file back to its pre-growth size; if the free that undoes the
  growth is itself refused, the extra clusters stay allocated and the
  caller — which already saw the growth fail — has no way to learn the
  rollback did too. `f.size` is set to the smaller value regardless, so the
  handle's own view of its length disagrees with what its chain still holds.
- `remove` (`fs.rs:932`) erases the directory entry first, by design (its own
  doc: freeing before erasing is the ordering `append_cluster` and
  `create_dir` argue against, since a live entry naming freed clusters is a
  cross-link waiting to happen). But that means once `erase_entries` lands,
  `free_chain`'s refusal is terminal: the entry is gone, so a caller that
  retries `remove` on the same path gets `NotFound`, never a chance to
  finish freeing the chain it once named.

Both are the FAT case of the shape
`issues/filesystem/a-refused-delete-has-already-thrown-the-file-away.md`
names for `BcacheFsAdapter::delete` — an earlier step that destroys state a
retry needs, ahead of the one device write that can fail — but that file is
about `bcachefs`, shares no code with `toyos-fat32`, and does not cover
either site here. Leaked clusters from this path are a `toyos-fat32-check`
finding ("N cluster(s) ... marked allocated and no directory entry reaches
them"), the same signature
`issues/filesystem/a-refused-link-write-leaks-the-cluster-append-cluster-just-claimed.md`
carries for the link-write case; this file is the free-side sibling, not the
one that file already tracks.

## Seen: the `rollback_to` arm splits the FATs under `quiesce_leaves_the_volume_whole`

Where the stop lands inside a `logd` `fsync` attempt that has already grown
the file on both FATs (the interleaving
`a-refused-link-write-leaks-the-cluster-append-cluster-just-claimed.md`
forces), the refused attempt's `rollback_to` writes its free's mirror half and
is refused the active half. On PR #506, which only moves that interleaving
earlier, `toyos-fat32-check` read the split off the volume the stop left: `FAT 1 differs from FAT 0 at entry 46: 0x00000000
against 0x0000002F` beside `4 cluster(s) from 46 are marked allocated and no
directory entry reaches them` (a free's mirror half), and in the earlier A/B
`FAT 1 differs from FAT 0 at entry 45: 0x0FFFFFFF against 0x0000002E` (a
`truncate_chain`'s mirror half). The split is worse than the leak for the
reason the link file gives: nothing says which copy is true.

## Owner

`toyos-fat32`, the crate that owns `rollback_to`, `remove` and `free_chain`.
A fix here has to hold to the same constraint the link-write file states: a
`BudgetExpired` refusal does not guarantee the write never landed (see that
file's "A refused write may already be on the medium"), so a retry cannot
assume the mirror-first `set_fat_entry` order left the active FAT untouched
without re-reading it.

## Exit condition

A host test in `toyos-fat32/tests/` that refuses the device write inside
`free_chain` once — one arm through `rollback_to` (a growth that fails and
whose rollback's free is then refused), one through `remove` (erase, then a
refused free) — and shows either that the retry path recovers the chain
without a cross-link, or that the failure is surfaced to the caller instead
of being silently discarded, with `assert_fats_agree` and
`toyos-fat32-check` green after. Until then this is `let _` and a discarded
error, not a decided policy.
