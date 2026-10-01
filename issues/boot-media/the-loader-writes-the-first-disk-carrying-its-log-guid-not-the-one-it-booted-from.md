---
status: open
kind: defect
opened: 2026-09-27
---

# The loader writes the first disk carrying its log GUID, not the one it booted from

`loaderlog::volume_handle` (`bootloader/src/loaderlog.rs`) takes the first
filesystem on the machine whose GPT partition's unique GUID is the log
partition's, and `loader.log` and the attempt records are written there.
`src/image.rs` draws every unique GUID once per image, so every stick written
from one image carries the same ones. With two such sticks plugged in, the
loader can write the log partition of the stick it did not boot from: a disk
it was not given.

Owned by stage 2 of
`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`,
which finds every volume the loader opens on the boot disk.

**Exit condition.** The loader writes only the device it booted from: the log
volume is looked up on the device of the loaded image's own handle, and a boot
with two copies of one image attached writes the other copy's bytes not at
all.
