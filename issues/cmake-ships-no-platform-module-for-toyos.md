---
status: open
kind: tooling
opened: 2026-10-01
---

# CMake ships no platform module for ToyOS, and each C sysroot carries one

CMake knows a system by its `Modules/Platform/<name>.cmake` and ships none for
ToyOS. Each C sysroot carries `Platform/ToyOS.cmake` and
`Platform/ToyOS-Initialize.cmake` beside the `toolchain.cmake` that puts them
on CMake's module path (`src/clang.rs`, `CMAKE`). No upstream merge request
carries them, because none is sent for now.

**Exit**: CMake ships ToyOS's platform module, and the C sysroot carries only
its toolchain file.
