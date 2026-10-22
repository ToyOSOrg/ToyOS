---
status: open
kind: track
opened: 2026-09-26
---

# ToyOS has a desktop

Owner rulings, 2026-09-26:

- **The desktop is part of ToyOS.** It ships in the image as a fully fledged
  desktop distribution; it is never a package the package manager installs.
- **ToyOS's own desktop software uses iced only.**
- **The platform supports iced alone, for now** (owner ruling, 2026-09-26):
  slint and egui are not supported
  (`issues/slint-and-egui-are-not-supported-by-the-owners-choice.md`).
- **ToyOS is a first-class platform, never Linux-compatible.** A crate that
  assumes "any other OS is Linux" gets a forked native toyos branch, carried
  until it is upstreamed.
- **Licences:** everything shipped as part of ToyOS is licence-clean, checked
  by a licence gate being built in parallel. Installable third-party apps are
  exempt.
- **Rendering:** CPU rendering is the first-class path; there is no CPU
  Vulkan. Later, a display-only driver for the T14's Iris Xe (Gen12 Xe-LP),
  then a Vulkan driver so wgpu apps get the GPU.
  Owner release-scope ruling, revised 2026-10-18: one active monitor with
  software rendering. Userland display-control drivers are allowed for refresh
  synchronization, page flips and display power/resume, superseding GOP-only
  output. GOP boot display and unaccelerated virtio-gpu remain supported. GPU
  rendering and multi-monitor support remain later work; this does not authorize
  hardware video decoding. Software-rendered Chromium and sleep/resume still need feasibility evidence.
- **Window management:** both floating and tiling.
- **Look and feel:** the familiar style every desktop shares — a taskbar with
  pinned and running apps together, one start menu with search, clear window
  chrome, snapping a window to an edge or half the screen. Minimal and
  discoverable, not a copy of Windows 7, and no glass or blur, because
  rendering is on the CPU.
- **COSMIC:** porting it whole, and `libcosmic` with it, is rejected (owner
  ruling, 2026-09-26). `libcosmic` carries System76's own forks of iced and
  winit, is git-only, and pulls about 303 crates at its leanest against plain
  iced's 97. COSMIC's own apps are GPL-3.0 and built only on Ubuntu, so they
  are not a realistic installable-package target either. ToyOS writes its own
  desktop environment on plain upstream iced; its ambition is to be a real
  competitor to COSMIC, not a port of it. `cosmic-text` (MIT OR Apache-2.0)
  remains in use.

## Background work

Owner release-scope decision, 2026-10-18: a desktop/display-service crash may
end the affected desktop session, potentially losing unsaved work. Application
reconnection and recovery across a desktop restart are deferred. Safe session
cleanup and reporting remain required; failure of the desktop alone is not
authorization to reboot or terminate unrelated work. Recovery controls when the
desktop is unavailable remain to design.

Owner decision, 2026-10-16: programs may continue running without windows,
including after their last window closes, without separate background
permission. This applies to CLI jobs too; window count does not determine
execution authority. Make running applications and background work visible,
with clear controls to stop them. Closing a window and quitting an application
are distinct actions. Application resource accounting and existing capability
limits still apply; background execution grants no additional access.

Owner decision, 2026-10-16: an installed application needs explicit user
approval to start automatically at sign-in. Remember the choice and let the
user change it in settings. Installation alone does not approve startup.
Requests and stored choices belong in the trusted userland permission and
session-launch path; the package manifest does not itself grant authority.
This policy does not govern the image's system services.

Desktop sign-out ends that session's work under
`issues/a-user-is-a-home-tree-and-a-login-row.md`. Terminal job lifetimes and
the concrete app-wide quit mechanism still need design; allowing background
execution does not promise that every job survives those events.

## Desktop notifications

Owner decision, 2026-10-16: apps need approval before showing desktop
notifications. Ask through the common runtime-consent path and remember the
decision; a recorded denial does not trigger repeated prompts. The user can
change it in settings. Messages inside an app's own windows are unaffected.
Use the shared saved-decision lifecycle for verified updates and uninstall.

## Camera and microphone privacy

Owner decision, 2026-10-16: camera and microphone are separate per-app
permissions. Ask through the common runtime-consent path and remember each
approval until removed; do not ask again for every recording session. Removing
saved approval affects future direct grants and does not stop already-authorized
recording, following
`issues/permissions-and-capabilities-need-one-runtime-contract.md`.
Use its verified-update and uninstall rules for saved decisions too.

The desktop shows which app is actively using each sensor, including background
use. Activity must come from the service enforcing capture access, not from an
app's claim to be recording or merely having saved permission. The capture and
activity-reporting protocols remain design work. This does not change the
separate per-session selection rule for screen sharing below.

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
3. **Core apps**: files, terminal and settings. Owner refinement, 2026-10-18:
   the minimal base omits a text editor and SSH, and includes graphical package
   management as well as its CLI. Optional applications are downloadable;
   the login greeter
   waits on the users track
   (`issues/a-user-is-a-home-tree-and-a-login-row.md`).
4. **Compositor services**: clipboard, drag and drop, notifications and
   screenshots. Owner decision, 2026-10-16: an ordinary application receives
   clipboard contents when the user pastes them; focus alone does not authorize
   a read. Background reading, including by clipboard managers, needs separate
   approval under the package track's request ∩ grant ∩ ceiling rule
   (`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`).
   The compositor's paste shortcut and separate clipboard-watcher connection
   are foundations; trusted paste-menu actions and toolkit integration still
   need a design. An application claiming a paste action is not proof of user
   intent. Screenshot access also follows the package permission rule.
   Owner decision, 2026-10-16: ordinary screen sharing and recording require
   the user to choose a window or display through trusted UI each time sharing
   starts. Grant capture of only that target for that sharing session; access
   ends when sharing stops. Do not turn a prior session's approval into
   remembered permission to start another. The capture protocol, target
   lifetime, buffer ownership and stop semantics remain design work; this
   ruling does not implement them or decide unattended remote access.
5. **Fractional scaling**: `issues/the-desktop-draws-at-the-panels-scale.md`.
6. **Accessibility**: owner decision, 2026-10-18: keyboard-only navigation is
   required for the first release. Screen-reader support, adjustable text size
   and other accessibility features are outside that initial requirement.
   Accesskit and high contrast remain later candidates, not release blockers.
7. **Input**: the T14 touchpad
   (`issues/the-t14-touchpad-is-i2c-hid-and-unbuilt.md`), scrolling
   and gestures, and input methods building on `toyos-keymap`.
8. **Laptop basics**: battery, lid, brightness and volume keys, suspend,
   Wi-Fi status; multiple monitors deferred beyond the first release.
9. **One visual design**: theme, icons (licence-clean sources) and spacing.
10. **A performance budget**: 60 fps at the T14's resolution with CPU
    rendering, measured by a test.
11. **Screenshot tests**: golden images per app.
12. **Display control and later GPU rendering**: display-only drivers may
    support first-release refresh and power management. Vulkan and accelerated
    rendering remain beyond the first release.

## Standing

Every unmodified guest app the platform stage claims to carry is a running
test, not a claim in this file; a stage's exit is measured, never asserted.
