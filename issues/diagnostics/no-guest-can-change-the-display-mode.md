---
status: open
kind: defect
opened: 2026-08-27
---

# `SYS_GPU_SET_RESOLUTION`'s success path runs on no machine the suite boots

Every guest `tests/common/qemu.rs` describes takes the UEFI GOP display path:
no profile attaches a virtio-gpu device, and `Shape::vga` is `"std"` throughout.
`GopGpu::set_resolution` answers `NotSupported` — GOP cannot change mode after
boot services exit — so the whole of what a successful resize does is unexecuted
by every test in this tree.

## Measured

Staged at `register_gpu` on 2026-08-27 and read off the boot log of
`diskless_boot`:

```
md2 probe: the resize answered Err(NotSupported) and the registry says Some((2048, 2048))
```

## What that leaves uncovered

Everything past the driver's refusal: the new framebuffer's allocation and the
old one's release (`virtio_gpu.rs`), the panic console's detach-and-rearm window,
the mode-change update of `device::set_framebuffer_info` and of the pointer's
per-axis scale, and the compositor's own re-read of the returned `GpuInfo`. A
defect in any of those is invisible here and visible on the owner's desktop,
which is the one machine that runs virtio-gpu.

## What would close it

A profile that attaches `virtio-gpu-pci`, and a guest that claims the
framebuffer, resizes, and compares what the call returned against what a second
claim is told. It is a new registered name and a new profile, which is why it is
filed rather than done beside the fix that needed it.
