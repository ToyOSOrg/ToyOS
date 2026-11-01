---
status: open
kind: defect
opened: 2026-10-22
---

# A `dlopen` image is one physically contiguous run

Every other user memory is 4 KiB frames in any order, but a library `dlopen`
loads is copied into one object over a physical run of 2 MiB pages
(`kernel/src/elf/mod.rs`, `Run::new`, over `SharedMemObject::create_run`):
the kernel lays the image out as one slice it relocates in place. A large
library on a machine whose memory is fragmented can be refused for want of a
run that long, though the frames exist, as a large program was before its
image object became scattered frames.

Owner: the memory track
(`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`).
Exit: a library image is laid out and relocated through scattered frames, and
`create_run` serves only memory a device is lent.
