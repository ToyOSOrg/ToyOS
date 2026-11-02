---
status: open
kind: finding
opened: 2026-11-02
---

# Dropping an in-flight bcachefs operation does not release its grants

`Mount::transact` in `lib/bcachefs/src/engine/mount/mod.rs` acquires a
journal/cache grant and then awaits `prepare`. Dropping that future during
device I/O drops the writer slot, but does not call `finish`: the grant,
cached-node holds, and any `Txn::allocated_data` cleanup remain outstanding.
The reclaim path also retains captured or writing nodes if its future is
dropped between planning, node I/O and pointer publication.

This is a code-inspection finding; cancellation has not been injected. The
fileserver currently runs these operations to completion with `block_on`.
Unused `Reservation` drop and returned operation errors have separate
cleanup paths and are not this finding.

The owning component is the bcachefs engine. A future cancellation change
must test dropping at each awaited boundary, including already completed
node writes, and establish either retryable ownership or an explicit stopped
mount without leaving admissions waiting forever. It must preserve reserved
DATA slots, reader barriers and the original journal dependencies.
