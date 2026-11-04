---
status: open
kind: defect
opened: 2026-11-04
---

# A small bcachefs journal makes reclaim write nodes after nearly every transaction

**Owner.** The storage track, which owns `lib/bcachefs`.

Each dirty node reserves its worst-case pointer update in the journal, and an
ordinary transaction is admitted only with a transaction's journal room left
(`lib/bcachefs/src/engine/mount/mod.rs`). Where the journal holds little, as
on the 64 MiB host test images and QEMU-sized DATA, a few dirty nodes fill
it, so reclaim writes nodes after nearly every transaction and the eager
put-in-place of #1152 gains nothing there. mkfs sizes the journal at 1/128
of the device, at least eight buckets and at most 32 MiB (`JOURNAL_BYTES`
in `mkfs.rs`), so the benches' DATA partitions are far from this; only
DATA mounts this engine read-write.

**Evidence.** The 64 MiB host images' tests: a crash after 2 000 creates
replays about a thousand journal keys of them
(`tests/admission.rs`, `the_first_transaction_after_a_replay_takes_each_delta_once`),
the rest already written by reclaim.

**Exit.** A journal reservation for dirty nodes that does not grow with
each one, for instance one pointer update for each reclaim batch rather than
for each node, with the reclaim interval on a small volume measured before
and after.
