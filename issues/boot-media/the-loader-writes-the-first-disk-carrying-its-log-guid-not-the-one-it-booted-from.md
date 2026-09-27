---
status: open
kind: defect
opened: 2026-09-27
---

# The loader writes the first disk carrying its log GUID, not the one it booted from

`loaderlog::volume_handle` (`bootloader/src/loaderlog.rs`) takes the first
filesystem on the machine whose GPT partition's unique GUID is the log
partition's, and `loader.log` and the attempt records are written there. Every
copy of one image carries the same partition unique GUIDs, and the image
release (`src/imagerelease.rs`) is written byte for byte to every stick made
from it. With two sticks of one release plugged in, the loader can write the
log partition of the stick it did not boot from: a disk it was not given, and
one that carries no TOYOS-DATA partition. The release notes name this as the
one such disk a boot may write.

Owner: the bootloader.

**Exit condition.** The loader writes only the device it booted from: the log
volume is looked up on the device of the loaded image's own handle, and a boot
with two copies of one image attached writes the other copy's bytes not at
all.
