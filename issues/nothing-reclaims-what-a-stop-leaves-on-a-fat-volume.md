---
status: open
kind: defect
opened: 2026-09-26
---

# Nothing reclaims what a stop leaves on a FAT volume

`toyos-fat32` undoes or carries through every refused write before the next call
proceeds, but that repair lives in memory (`toyos-fat32/src/repair.rs`). A
machine that stops while one is queued, or between two writes of any call,
leaves what `a_stop_at_any_write_leaves_only_the_named_windows`
(`toyos-fat32/tests/refused_writes.rs`) admits: orphaned clusters no more than
one call claims and frees, at most one FAT entry whose two copies differ, and at
most one partial long-name run. `toyos-fat32-check` names each. Nothing in
ToyOS repairs them: mounting writes nothing by design, the log partition has no
`fsck`, and a host `fsck` is a macOS binary.

A split entry asks which copy is true, and the answer is known: each holds a
state the call or its repair passed through, so copying the active entry over
the mirror loses at most the stopped call's last write.

## Exit condition

Two steps, the first standing on its own:

1. Mount detects each of those states without writing anything — a FAT entry
   whose copies differ, a run of allocated clusters no entry reaches, a
   long-name run no short entry completes — and logs each by name, so a volume
   a stop left is known as one before anything is written to it.
2. A volume a stop left in any of those states is consistent again after the
   next mount's first write, or the owner rules the leak acceptable and this
   is a `rejected` file saying so.
