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
landed the first part: the loader hands the panel's EDID over from the GOP
handle that shows the scanout, `lib/edid` decodes it, `toyos_desktop::Scale::auto`
chooses the scale, and the compositor logs it and answers it in
`inspect display`. Nothing draws at it.

What is left:

1. **The rest of the scale.** The `display` port, served by the compositor and
   received by `toybox` alone (`build/build.rs`'s `HELD_ALONE`), carrying
   `screen scale <step>|auto`, kept in `/state/compositor`. The window
   protocol's `WindowInfo` and `ResizeInfo` carry the scale as `n/120`, a
   create request says whether its size is logical or physical, and
   `toyos_window::Window` answers `scale_factor()`. The compositor draws its
   chrome, glyphs (from the TTF, in `toyos-font`), icons and cursor (one
   rasterizer, `sdk/icon`, replacing `sprite` and `files`' copy) at the scale,
   relays windows out on a change, and holds two buffers per window across a
   resize, both counted by `budget::window_bytes`. terminal, filepicker and
   `files` render at the scale; `editor` and `paint` stay at 1 until they read
   it in their own repositories. One commit on the winit fork's
   `toyos-0.30.13` answers `scale_factor()` and sends `ScaleFactorChanged`.
2. **virtio-gpu leaves the kernel** (stage 1 of
   `issues/every-driver-is-still-in-the-kernel.md`): the compositor drives it
   through a `pci:1af4:1050` claim and follows the host window's display
   events. Blocked on `issues/a-cargo-run-boot-claims-every-pcidev-slot.md`:
   the Virtio profile's fifth claim is refused until a slot is free.

Constraints already paid for:

- **The panel's size comes only from the EDID**, read before
  `ExitBootServices`. On the T14 the protocol is on the first of two GOP
  handles, on the AMD bench on the second; the panels are 309x173 mm and
  309x174 mm at 1920x1080, and the rule gives both 1.5 (`lib/edid/fixtures/`).
- **QEMU's OVMF publishes no EDID protocol**, so every GOP guest runs at 1.
- **QEMU writes no cursor-queue answer** (used `len` 0) and its `GET_EDID`
  `size` field is the buffer's 1024, measured on #895's evidence branch
  `pr-895` at `7d47ad2b`.

Exit: the desktop and its own clients draw at the scale on both benches and
under QEMU's virtio-gpu, a host window's resize reaches the guest's mode, and
no `SYS_GPU_*` number is left in `sdk/abi/src/syscall.rs`.
