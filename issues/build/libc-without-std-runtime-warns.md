---
status: open
kind: defect
opened: 2026-09-27
---

# libc without `std-runtime` warns

`userland/libc` built as the `staticlib` a C program links — without
`std-runtime`, the configuration `src/libc.rs`'s `build_c` makes for every C
sysroot — carries a warning: `src/memory.rs`'s `backend` module imports
`super::*` and uses nothing from it. `userland/`'s `-Dwarnings` refuses it, so
`build_c` runs cargo from the repository root, where no configuration denies
warnings, and the warning is printed and let through. The `std-runtime` build
has no such module and builds under `-Dwarnings`.

**Exit**: libc builds warning-free without `std-runtime`, and `build_c` runs
from `userland/` like `build` does.
