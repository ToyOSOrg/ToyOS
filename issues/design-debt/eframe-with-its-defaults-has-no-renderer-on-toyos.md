---
status: open
kind: defect
opened: 2026-09-26
---

# eframe with its defaults has no renderer on ToyOS

`eframe = "0.34"`, egui's own app framework, draws only through glow (OpenGL)
or wgpu, and ToyOS has no GL and no wgpu adapter; its defaults also bring
`arboard` and `webbrowser` through `egui-winit`, which do not build here. An
egui app runs today only through a third-party CPU runner such as
`egui_software_backend`, which is what `userland/egui-hello` uses.

Exit condition: an unmodified eframe app with default features builds and
draws on ToyOS — a CPU renderer eframe can fall back to, or a wgpu adapter —
with a guest test that runs one.
