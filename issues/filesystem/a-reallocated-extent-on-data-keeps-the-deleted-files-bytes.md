---
status: open
kind: defect
opened: 2026-09-05
---

# A block freed by a delete and handed to the next file keeps the deleted file's bytes on the device

Delete a file on DATA, write a new one large enough to be given the block that
just came free, shut down, and read the new file off the image on the host: its
first extent still holds **the deleted file's content**. The guest reads the new
file correctly in the same boot — it executed it — so the two views disagree and
the device's is the wrong one.

## Reproduction

Measured in `pkg_install_gbae`'s guest on 2026-09-05, with the probes in an
order that has since been changed (see the exit condition).

1. `Profile::Metal`, DATA on NVMe, formatted for this run; confirm `/apps and
   /home are a tmpfs` is **not** in the boot log.
2. Create `/apps/toy` and a symlink `/apps/toy/echo -> /system/bin/toybox`.
3. Delete both (`/system/bin/pkg remove toy`, which unlinks then `SYS_RMDIR`s).
4. Install a 1,608,600-byte binary at `/apps/gbae/gbae`.
5. Launch it — it runs, so the guest's own read of the path is right.
6. `run shutdown`, then read `apps/gbae/gbae` off the image with
   `tests/common/storage.rs`'s `FileBlocks`.

The length is right and byte 0 is not:

    apps/gbae/gbae is 1608600 bytes on the device against the archive's 1608600,
    first differing at 0: device 2f73797374656d2f62696e2f746f7962
    against archive 7f454c46020101000000000000000000

`2f73797374656d2f62696e2f746f7962` is `/system/bin/toyb` — the first sixteen
bytes of the symlink target written at step 2 and deleted at step 3. Reproduced
on both arms of one run (the parallel one and the `ALONE` re-run), byte for
byte.

## What it is not

Not
`issues/filesystem/a-page-faulted-through-an-old-backing-is-nobodys.md` either:
no mapping is taken across the write here.

Which half is wrong is not established. Either the allocator handed out a block
whose old contents were never overwritten and the new file's first extent was
never written back, or the extent the host reader resolves for the new file is
the freed one. The kernel's own `Syncing filesystems` line is in the shutdown
capture either way.

## Exit condition

A test deletes a file, writes another that reuses its blocks, and finds the new
file's bytes on the device. `pkg_install_gbae` runs its symlink and removal
probes *after* the install it reads back, so that judge is about packages and
not about this; the ordering is stated at that site and comes out when this
closes.
