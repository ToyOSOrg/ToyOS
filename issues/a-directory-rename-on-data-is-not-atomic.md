---
status: open
kind: defect
opened: 2026-10-09
---

# A directory rename on DATA is not atomic

Fileserver's. `userland/fileserver/src/data.rs`'s `rename` of a directory
renames each entry under it one at a time, because the format keys every file
by its whole path and has no rename of a prefix; its own comment says a kill
in the middle leaves the directory in two halves. A refused write does the
same without a kill: a host test of `DataVolume` (scratch, not committed) made
`home/staged/a` (5 bytes) and `home/staged/z` (240 pages on a fragmented
volume), and renamed `home/staged` to a 300-byte name. `rename` answered
`ResourceExhausted` (the format's `EntryTooLarge { size: 4192, max: 4064 }`
for `z`), and the volume then held `<to>/a` and `home/staged/z`: half the
directory under each name, and the call reported as failed. The format keeps
no journal either (`issues/bcachefs-crate-is-not-bcachefs.md`), so even one
entry's rename is only as whole as the sync that writes it.

**What it blocks.** The package track's stage-then-commit
(`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`):
`/system/bin/pkg` stages `/apps/<name>` privately and commits it in one step,
which a rename that can leave half a package under `/apps` is not.

## Exit condition

A directory rename on DATA leaves the directory whole under exactly one of its
names whatever write is refused and wherever the server is killed, measured by
a test that refuses every write of the rename in turn and kills the server at
every one.
