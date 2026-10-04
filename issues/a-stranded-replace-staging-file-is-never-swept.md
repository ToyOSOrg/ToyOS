---
status: open
kind: defect
opened: 2026-09-01
---

# A `.toyos-replaced-*.tmp` left by a failed rollback is never swept

`Fat32::replace_rename` (`toyos-fat32/src/fs.rs`) stages an existing
destination under `.toyos-replaced-{sequence:08x}.tmp` and frees the staged
entry only once the move commits. When the move *and* its rollback both fail
the staging entry stays on the volume; `ReplaceFailed::stranded` now names it,
so the destination's data is reachable again, but nothing removes it.

`replacement_temporary` picks the first sequence whose name is free, so each
leftover consumes one of `MAX_DIR_ENTRIES` sequences permanently and every
later replacing rename in that directory pays one more `exists` probe. The
volume stays structurally valid, so `toyos-fat32-check` says nothing about it.

Whoever sweeps has to decide what a `.toyos-replaced-*.tmp` means at mount
time: it is either a destination somebody still wants or a leftover of a
rename that failed in a boot that is over, and the volume does not record
which. A sweep that deletes the first case loses the data the staging order
exists to protect.
