---
status: open
kind: defect
opened: 2026-09-03
---

# The second scanout buffer is read by nobody

`alloc_framebuffer` in `kernel/src/drivers/virtio_gpu.rs` allocates two whole
framebuffers of contiguous 2 MiB pages per mode and mints both as
`FramebufferInfo::scanout`; the device is given `scanout[0]` only, and so is
every reader — `userland/compositor/src/session.rs` (lines 154 and 949) and
`userland/console/src/main.rs` (line 99) adopt `scanout[0]` and nothing adopts
`scanout[1]`. Exit: the second buffer is deleted, or something reads it.
