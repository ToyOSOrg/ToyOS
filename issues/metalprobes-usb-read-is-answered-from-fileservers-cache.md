---
status: open
kind: defect
opened: 2026-09-27
---

# metalprobe's USB read is answered from the file server's cache, not the stick

`tests/guest/metalprobe/src/usb.rs`'s `read` writes its file to `/log`, closes it,
and times reading it back as "a cache miss the stick has to answer" — true
while the kernel's write-back dropped a closed file from its cache. `/log` is
served by a file server now, whose block cache
(`system/fileserver/src/cache.rs`) keeps up to 16 MiB of clean blocks and
drops nothing at a close. So the timed read is the
file server's memory, and the `usbread` span is no longer the stick's.

**Exit**: the timed read is one the stick answers — a file the file server has
not read since it started, or a read through a block session that bypasses the file
server.
