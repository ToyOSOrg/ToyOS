---
status: open
kind: finding
opened: 2026-08-31
---

# A mode change writes the framebuffer registry twice

`SYS_GPU_SET_RESOLUTION` dispatches to `crate::gpu::set_resolution`, which ends
in `crate::device::set_framebuffer_info(screen(&new_info))`
(`kernel/src/gpu.rs:99`), and then to `sys_gpu_reset_scanout`, which opens with
`device::set_framebuffer_info(screen.clone())`
(`kernel/src/arch/syscall/device.rs:137`). `gpu::set_resolution` has exactly one
caller — `dispatch.rs:423`, whose `Ok` arm is that second call — and both build
a `device::Screen` with the same `width`, `height`, `stride`, `pixel_format`,
`flags` and the same three `Region`s, with `scanout`/`cursor` handles left
`HANDLE_INVALID`. So the second write lands on the first with the same bytes,
and `mouse::set_screen` runs twice with the same geometry.

## How it surfaced

Found while building `gpu_set_resolution`'s negative control (2026-08-31 wave-4
bundle F): deleting either call alone left the test green, because the other one
already updated the registry. A break that takes two edits to stage is a
duplicate, and it is the reason the committed control mutates both lines at
once.

No wrong behaviour today: the two writes are equal, so whichever lands second
leaves the registry right. What it costs is a defect in *one* of them being
invisible — the surviving call answers for both.

## What would close it

One writer. `gpu::set_resolution` publishing `INFO` and leaving the registry to
its caller is the smaller change; the alternative is the caller not writing what
`gpu::set_resolution` already did. Either way `gpu_set_resolution`'s negative
control becomes a one-line mutation, which is what says the duplicate is gone.
