---
status: open
kind: finding
opened: 2026-09-18
---

# A refused link write in `append_cluster` leaks the cluster it just claimed, and the caller's retry does not take it back

Read, not measured. `toyos-fat32/src/fat.rs`'s `append_cluster` is two FAT
updates: `alloc_cluster` claims a free cluster and terminates it, then
`set_fat_entry(last, new)` links it. Its doc says a failure between the two
"leaks a cluster rather than producing a chain that runs into free space", and
that such a leak is recoverable by `fsck`.

The kernel retries instead of running `fsck`. A budget expiry
(`IoError::BudgetExpired`) on the link write returns through `Fat32::write`,
whose `rollback_to` calls `shrink_chain` — and that walks the chain from the
file's first cluster through the *active* FAT, where the link never landed. It
reaches `last`, finds it terminated, and frees nothing: the new cluster stays
allocated and unreachable. `alloc_cluster` had already advanced
`fsinfo.next_free` past it, so the caller's next attempt — `SYS_FSYNC`'s ladder
in `kernel/src/object/ops.rs`, or `writeback::drain_retrying` — claims a
different cluster and succeeds. The volume then carries one allocated cluster no
directory entry reaches, per refused link.

`set_fat_entry`'s mirror-first order makes the *allocation's* refusal idempotent
(the active FAT still reads the cluster free, so the retry re-picks it); nothing
makes the link's refusal so.

That is the sentence several names say in
`issues/build/a-loaded-suite-reds-a-volume-checker-on-both-arms.md`:
`N cluster(s) from M are marked allocated and no directory entry reaches them`
(`log_flush_retry`, `fat_backing_revoked`, `redirty_mid_flush`,
`toybox_cp_volume`) — on a loaded host, where a budget expiry mid-flush is what
load produces. Whether this is their cause is not shown.

## What would show it

A host test in `toyos-fat32/` over a `BlockAccess` that refuses the N-th write
with `BudgetExpired` once: append to a file so that the refused write is the
link's active-FAT entry, retry the write, flush, and run `toyos-fat32-check`
over the image. Red if the reading is right.

**Exit condition**: that test exists and is green — `append_cluster` frees the
cluster it claimed when the link is refused (best effort, on the device that
just refused), or the retry re-uses it — or the test is green as written and
this reading is wrong, in which case the file is deleted with the test as its
evidence.
