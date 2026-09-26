---
status: none
kind: rejected
opened: 2026-09-26
---

# slint and egui are not supported, by the owner's choice

Owner ruling, 2026-09-26: the desktop platform carries iced only, for now.
The slint fork, the parley/fontique fork, and the slint and egui test apps
were removed with their `[patch]` rows, and nothing in the tree builds either
toolkit. What they needed was measured before the removal: slint through its
winit backend and software renderer, and egui through the third-party
`egui_software_backend`, each drew under the compositor; slint with its
default features did not build (Qt, glutin's OpenGL, accesskit's zbus), and
eframe with its defaults has no renderer here (glow or wgpu only).

**Exit**: revisit once the Rust target is upstream, so a toolkit's own
`target_os = "toyos"` arms can be proposed where they belong instead of
carried as forks.
