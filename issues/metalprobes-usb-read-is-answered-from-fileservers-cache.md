---
status: open
kind: defect
opened: 2026-09-27
---

# metalprobe's USB read is answered from the file server's cache, not the stick

`tests/guest/metalprobe/src/usb.rs`'s `read` writes its file to `/log`, closes it,
and times reading it back as "a cache miss the stick has to answer" — true
while the kernel's write-back dropped a closed file from its cache. `/log` is
served by a file server now, which keeps a file's pages in its paged object
past its close (`system/fileserver/src/pages.rs`), up to its pager's
capacity. So the timed read is memory, and the `usbread` span is no longer
the stick's.

**Exit**: the timed read is one the stick answers — a file the file server has
not read since it started, or a read through a block session that bypasses the file
server.
