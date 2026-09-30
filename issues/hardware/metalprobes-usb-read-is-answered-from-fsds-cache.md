---
status: open
kind: defect
opened: 2026-09-27
---

# metalprobe's USB read is answered from fsd's cache, not the stick

`userland/metalprobe/src/usb.rs`'s `read` writes its file to `/log`, closes it,
and times reading it back as "a cache miss the stick has to answer" — true
while the kernel's write-back dropped a closed file from its cache. `/log` is
served by `/system/bin/fsd` now, whose block cache (`userland/fsd/src/cache.rs`)
keeps clean blocks until it holds `CLEAN_LIMIT` of them and drops nothing at a
close. So the timed read is fsd's memory, and the `usbread` span
`tests/metal-profile.toml` prices is no longer the stick's.

**Exit**: the timed read is one the stick answers — a file fsd has not read
since it started, or a read through the partition claim that bypasses the
file server — with the profile row re-measured on the T14.
