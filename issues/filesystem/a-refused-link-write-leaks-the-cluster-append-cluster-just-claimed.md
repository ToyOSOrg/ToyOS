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

## T14 run 139 confirmed it

`lan_swap` at `d54d740b` swapped and stopped cleanly, but
`toyos-fat32-check` refused the log partition afterward: "1 cluster(s) from
34 are marked allocated and no directory entry reaches them". The dated
log's chain ran `10 … 33 → 35 … 135`; cluster 34 was end-of-chain in both
FATs, all zeros, reached by nothing, and cluster 33's text ended mid-word
where 35 picked it up. At 6.004 s, `usb-storage`'s write of 4 KiB block
23297 — the log partition's first FAT sector, holding clusters 33 and 34's
entries — ran out of its operation budget; at 7.087 s the kernel reported
the dated log's write refused with `BudgetExpired`. `append_cluster` had
claimed 34 and linked `33 → 34`, the link's active-FAT half was refused,
`rollback_to`'s `shrink_chain` found 33 already terminated and freed
nothing, and logd's retry claimed 35 from the advanced FSInfo hint. This is
exactly the leak this file predicted by reading the code, reproduced on
real hardware.

A fix was attempted at `10cdcc39` (PR #492) and reverted: it held the
claimed-but-unreached cluster and re-drove the next claim onto it before
scanning, freeing it at `sync` if nothing came. Review
(https://github.com/ToyOSOrg/ToyOS/pull/492#issuecomment-5840276317) found
it trades the orphan for worse failures. Any fix must meet what the review
exposed:

- **A refused write may already be on the medium.** `served`
  (`kernel/src/drivers/xhci/wait/msc.rs`) can issue the op, get `Device`
  back with the disk held, and only then answer `BudgetExpired`; a re-issued
  op on `Back` can also come back refused after landing. The fix's premise —
  that a `BudgetExpired` write left the active FAT exactly as it was — does
  not hold in general. A held claim's chain state must be re-verified
  (re-read the entry, not assumed) before it is reused or re-terminated.
- **`create_dir`'s claims nest.** Directory growth's own `claim_reached`
  (for the new directory cluster) is called from inside `create_dir`'s outer
  `claim_reached` (for the directory entry's write). A single "at most one
  unreached claim" field cannot hold both; the inner claim is overwritten
  and lost the same way the outer one used to be.
- **`sync`'s own free of a held claim can itself be refused.** Freeing
  writes the mirror FAT first; if the active half is then refused, the
  mirror reads the cluster free while the active FAT still reads it
  end-of-chain — the two FATs split for good, with no fsck to reconcile a
  log partition. This is worse than the orphan: a leaked cluster only wastes
  space, a split FAT is undefined which copy is true.
- **The window is in memory only.** Any state a fix keeps between "claimed"
  and "reached" or "freed" does not survive a reset, panic or power loss
  between them; FAT has no journal, so this cannot be closed, only bounded
  and stated.

This is `issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`
stage 3's neighbourhood: the refusal chain a fix here has to reason about
(deadline slots, retries and backoff, `BudgetExpired`, writeback re-enqueue)
is the kernel-side budget machinery that stage moves into a userland block
service entirely. A fix that survives contact with a real refusal may be
simpler to write once the write path is straight-line userland code than as
another case bolted onto the kernel's busy-waiting VFS.

## What would show it

A host test in `toyos-fat32/` over a `BlockAccess` that refuses the N-th write
with `BudgetExpired` once: append to a file so that the refused write is the
link's active-FAT entry, retry the write, flush, and run `toyos-fat32-check`
over the image. Red if the reading is right. Given the review's findings
above, the test suite also needs a mode that performs the write and *then*
returns `BudgetExpired` (the write landed), a nested `create_dir` case, and a
refused sync-time free that leaves the FATs split — each asserted with
`assert_fats_agree` and `fsck`, not just the single-orphan check.

**Exit condition**: that test (and its siblings above) exist and are green —
`append_cluster` frees the cluster it claimed when the link is refused (best
effort, on the device that just refused), or a re-verified retry re-uses it,
without cross-linking a chain or splitting the two FATs — or the tests are
green as written and this reading is wrong, in which case the file is
deleted with the tests as its evidence.
