---
status: open
kind: defect
opened: 2026-09-05
---

# pkg keeps a tree walk for a `remove_dir_all` that no longer leaves the directory

std's `remove_dir_all` (`sdk/std/sys/fs.rs`) once emptied a directory and
returned `Ok(())` without removing it or any directory under it, so
`pkg remove gbae` reported success and the next `pkg install` of the same
archive answered `/apps/gbae is already there`. It now ends with `rmdir(path)`
at every level, as it has since the backend moved to `sdk/std` (`3831f291b`).

What is left is the workaround. `system/pkg/src/main.rs`'s `remove_tree` gives
that defect as the reason it is not `fs::remove_dir_all`, in a doc comment that
is now false. Its other reason may stand: it walks with an explicit stack where
std's recurses once per level.

**Exit**: `remove_tree` either goes for `fs::remove_dir_all`, or keeps a doc
comment that states only the reason that holds.
