---
status: open
kind: track
opened: 2026-09-26
---

# ToyOS has a desktop

The desktop's design was ruled by the owner on 2026-09-26: part of ToyOS and
never a package, iced alone for ToyOS's own software and for now for the
platform (`issues/slint-and-egui-are-not-supported-by-the-owners-choice.md`),
CPU rendering first, floating and tiling, and its own environment on plain
upstream iced rather than a port of COSMIC, whose `cosmic-text` (MIT OR
Apache-2.0) remains in use. Everything shipped as part of ToyOS is
licence-clean; installable third-party apps are exempt.

## Why CPU rendering, not a CPU Vulkan

A 2026-09-26 probe (`../toyos-uiprobe`, cut from `main` at `03b1b4db`, since
removed) built and ran unmodified `iced` 0.14, `slint` 1.18 and `egui` 0.34
guests in QEMU under the real compositor, each drawing with its own CPU
renderer. No toolkit's default path wants Vulkan: slint's own software
renderer and Skia are its non-GL choices, iced falls back from `iced_wgpu` to
`iced_tiny_skia`, and egui's CPU path is a third-party backend
(`egui_software_backend`). `wgpu` itself has no rasteriser of its own — it
only picks whatever the OS exposes (Vulkan, Metal, DX12, or a software
adapter like lavapipe) — so a CPU Vulkan would help only wgpu-first apps, and
would be slower for them than the purpose-built 2D CPU rasterisers (tiny-skia,
`vello_cpu`, slint's renderer) those toolkits already fall back to. The only
Rust attempt at a CPU Vulkan, Kazan, is dormant and never ran a real
application; the real ones (Mesa lavapipe, SwiftShader) are C/C++, which this
tree does not adopt. `vello_cpu` (Linebender, pure Rust, SIMD, multithreaded)
is the CPU-rendering stack to watch; its hybrid mode is the eventual path to
the GPU, through wgpu's Vulkan backend once one exists.

## Why iced for ToyOS's own shell, apps stay open to all three

The probe found no dependency-shaped blocker to any of the three toolkits: no
C, no `cc`, no `bindgen`, no fontconfig, no freetype, in any of their toyos
trees once two upstreamable cfg fixes land. The remaining blockers are the
platform layer, not the toolkit:

1. All three pin **winit 0.30**; ToyOS's winit fork is 0.31-beta only. A
   0.30 branch is needed regardless of which toolkit ships in the image,
   because third-party apps on any of them need it.
2. softbuffer's and slint's winit backend's catch-all `cfg(not(any(android,
   apple, redox, wasm, windows)))` matches toyos and pulls X11/Wayland/DRM
   crates that then fail to build — 8 lines in the softbuffer fork fix it for
   every toolkit at once, and are upstreamable.
3. ToyOS ships no system fonts, no TTF, only pre-rasterised `.font` files;
   slint panics and iced draws no text without one.
4. `EventLoopProxy` cannot wake a loop blocked in a window's `recv_event`, so
   async iced only progresses on the next compositor event — an SDK and
   `toyos-window` change, not a toolkit choice.

None of that favours one toolkit as the *guest* story; every one of the three
already runs. iced is chosen for ToyOS's own desktop software as an owner
ruling — its Elm architecture, tiny-skia CPU renderer and lighter tree (97
unique crates against slint's 206 and egui's 64, but slint's dependencies are
the heaviest to keep licence-clean and cfg-clean) fit a shell this tree
maintains itself. The later ruling above narrows the platform itself to iced
until the Rust target is upstream.

## Stages

1. **The platform: existing apps run unchanged.** winit 0.30's toyos arm,
   softbuffer's cfg fix, system fonts (fontdb's toyos arm, a real font
   shipped in the image), a wakeable event loop, and wgpu failing gracefully
   with no backend instead of panicking (so default-feature iced falls back to
   tiny-skia on its own). A guest test holds one unmodified `iced` app.
   **Exit**: it builds and runs in QEMU with zero dependency or source
   changes to the app crate itself.
   **In progress**, in a separate PR — not cited here by number.
2. **A shell**, written in iced: the compositor's window management for
   floating and tiling with edge snapping; a taskbar or panel with pinned and
   running apps; a start menu or launcher with search.
3. **Core apps**: files, terminal, editor and settings. The login greeter
   waits on the users track
   (`issues/a-user-is-a-home-tree-and-a-login-row.md`).
4. **Compositor services**: clipboard, drag and drop, notifications and
   screenshots. Clipboard reads and screenshots are privileges an app asks for
   at run time, granted within the image's ceiling under the package track's
   stage 6
   (`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`).
5. **Fractional scaling**: `issues/the-desktop-draws-at-the-panels-scale.md`.
6. **Accessibility**: accesskit, keyboard-only navigation, high contrast.
7. **Input**: the T14 touchpad
   (`issues/the-t14-touchpad-is-i2c-hid-and-unbuilt.md`), scrolling
   and gestures, and input methods building on `toyos-keymap`.
8. **Laptop basics**: battery, lid, brightness and volume keys, suspend,
   Wi-Fi status and multiple monitors.
9. **One visual design**: theme, icons (licence-clean sources) and spacing.
10. **A performance budget**: 60 fps at the T14's resolution with CPU
    rendering, measured by a test.
11. **Screenshot tests**: golden images per app.
12. **GPU**: a display-only Xe-LP (Gen12 Xe-LP, PCI ID 8086:9A49 on the
    tree's own T14) driver, then a Vulkan driver so wgpu apps get the GPU.

## Standing

Every unmodified guest app the platform stage claims to carry is a running
test, not a claim in this file; a stage's exit is measured, never asserted.
