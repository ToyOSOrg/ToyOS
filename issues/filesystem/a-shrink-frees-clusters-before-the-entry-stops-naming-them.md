---
status: open
kind: defect
opened: 2026-09-26
---

# A shrink frees clusters before the entry stops naming them

`toyos-fat32/src/fs.rs`'s `set_len` to a shorter length frees the chain's tail
at once and records the new size in the directory entry only at the caller's
next `flush_meta`. Between the two the entry names more bytes than its chain
holds, and for a shrink to zero it names a first cluster that is free — which
the next allocation hands to another file. `a_stop_at_any_write_leaves_only_the_named_windows`
(`toyos-fat32/tests/refused_writes.rs`) measures it by freezing the device
during a `set_len` then `flush_meta`: `DIR_FileSize is 2560 bytes, which needs 5
clusters, and the chain holds 2`. It needs no refusal: a machine that stops
between a successful shrink and its flush leaves the same volume.

The kernel's `truncate_to` and `update_metadata` (`kernel/src/fat32_adapter.rs`)
are the two callers that shrink.

## Exit condition

A shrink stopped at any write leaves at most a leak, and the truncate arm of
that test's `own` filter is deleted.
