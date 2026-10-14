---
status: assigned
kind: track
opened: 2026-10-14
---

# The desktop draws at the panel's scale, and follows a mode change

Held by the orchestrator. Stage 5 of `issues/toyos-has-a-desktop.md`, and
the owner's go for a display scale factor and live resize on QEMU and on metal
alike.

The accepted design, its three review rounds and the orchestrator's rulings
are on pull request #895: the design as accepted
(https://github.com/ToyOSOrg/ToyOS/pull/895#issuecomment-6158220132), the
reviews (#issuecomment-6131030591, #issuecomment-6154531219,
#issuecomment-6154671069) and the acceptance (#issuecomment-6154686668). #895
landed the panel's EDID and the scale chosen from it; the desktop, its own
clients and winit's `scale_factor()` draw at that scale, which `screen scale`
sets live.

What is left:

- **The benches' reading** of `gop_edid_scale`, which now also holds the
  desktop drawn at 1.5 and a client's window given its buffer at it. Nothing
  else: the compositor drives QEMU's virtio-gpu and follows a host window's
  resize (`the_compositor_drives_the_virtio_gpu`), and no `SYS_GPU_*` number
  is left.

Constraints already paid for:

- **The panel's size comes only from the EDID**, read before
  `ExitBootServices`. On the T14 the protocol is on the first of two GOP
  handles, on the AMD bench on the second; the panels are 309x173 mm and
  309x174 mm at 1920x1080, and the rule gives both 1.5 (`lib/edid/fixtures/`).
- **QEMU's OVMF publishes no EDID protocol**, so a GOP guest runs at 1 unless
  its image's compositor row names a scale (`cargo run --scale`).
- **QEMU writes no cursor-queue answer** (used `len` 0) and its `GET_EDID`
  `size` field is the buffer's 1024, measured on #895's evidence branch
  `pr-895` at `7d47ad2b`.

Exit: the desktop and its own clients draw at the scale on both benches and
under QEMU's virtio-gpu, a host window's resize reaches the guest's mode, and
no `SYS_GPU_*` number is left in `sdk/abi/src/syscall.rs`.
