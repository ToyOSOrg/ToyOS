---
status: open
kind: defect
opened: 2026-09-26
---

# A rename that stops between its writes leaves a chain two entries reach

`toyos-fat32/src/fs.rs`'s rename writes the new entry run, then erases the old
one, then repoints a moved directory's `..`. A refused write is undone before the
call returns, but a machine that stops between those writes — a reset, a power
loss, or a device that stops answering so the undo never lands — leaves the
state the order had reached. Measured by
`a_stop_at_any_write_leaves_only_the_named_windows`
(`toyos-fat32/tests/refused_writes.rs`), which freezes the device at every write
of a directory move and lists what `toyos-fat32-check` says: `cluster 5 is
already held by /into/A moved directory` (both entries live), `".." names
cluster 0; the format requires the parent's cluster 4` (moved, `..` not yet
repointed), and a `DotEntry` complaint on the doubly reached directory. Every
other call's stop leaves a leak, one split FAT entry or a partial long-name run;
rename's leaves a cross-link, which is the state FAT is worst at recovering
from.

The order is deliberate: erasing first opens a window in which neither name
resolves, which the rename's own doc calls worse. The trade-off is between a
leak-shaped stop (erase first: the chain is orphaned for a moment) and a
cross-link-shaped one (insert first), and it is not this file's to settle.

## Exit condition

A rename whose device stops at any of its writes leaves only states the test
above admits for every other call, and the rename arm of its `own` filter is
deleted.
