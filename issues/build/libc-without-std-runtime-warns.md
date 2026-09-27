---
status: open
kind: defect
opened: 2026-09-27
---

# libc without `std-runtime` warns

`userland/libc/src/memory.rs`'s `backend` imports `super::*` and uses nothing
of it, so `libc::build_c` runs cargo outside `userland/`, whose `-Dwarnings`
refuses the warning.

**Exit**: no warning, and `build_c` runs from `userland/`.
