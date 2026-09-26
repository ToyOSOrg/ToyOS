---
status: open
kind: defect
opened: 2026-09-26
---

# slint's default features do not build for ToyOS

`slint = "1.18"` with its default features pulls the Qt backend, femtovg over
OpenGL through glutin ("Please select at least one api backend"), the system
tray and accessibility, whose `accesskit_unix` brings zbus back. None has a
ToyOS arm. An app that asks for `default-features = false` and
`backend-winit` with `renderer-software` — slint's documented software setup,
and what `userland/slint-hello` uses — builds and runs, and one written with
the defaults does not, which is short of "existing Rust just works".

Exit condition: `slint` with default features builds for ToyOS and falls
back to its software renderer, through native ToyOS arms in the slint fork
(`forks.toml` `[slint]`), with a guest test running such an app.
