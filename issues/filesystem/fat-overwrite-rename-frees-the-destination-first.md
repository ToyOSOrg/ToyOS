---
status: open
kind: defect
opened: 2026-08-28
---

# A FAT overwrite-rename frees the destination's clusters before the move, so a device error during the move loses the destination

`FatFs::commit` (the `ReplaceRename` phase, `kernel/src/fat32_adapter.rs`)
emulates POSIX overwrite on FAT, which has no atomic replace: when the
destination exists it `delete`s it — freeing its clusters and erasing its
directory entry — and then calls `Fat32::rename` to move the source onto the
freed name. The source is validated present and *distinct* first, so the delete
never runs for a rename that will fail to find its source or that names the same
entry (a case-only rename onto itself). What remains: if `Fat32::rename` fails on
a **device error** after the delete, the destination is already gone and the
move did not happen, so an overwrite-rename that hits a transient device failure
loses the file it was overwriting and returns the error.

This is much narrower than the deterministic defect it descends from (a rename
with an absent or case-equal source, now refused before any delete): it needs a
real device failure between the delete and the move, and only on an overwrite —
a destination that pre-exists as a distinct file. Closing it needs a reversible
order FAT does not offer without larger work: rename the destination out of the
way to a reserved temporary name, move the source, then free the temporary last;
or a small journal. Recorded rather than fixed because the reversible order is
its own change with its own two checks.
