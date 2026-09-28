---
status: open
kind: tooling
opened: 2026-09-28
---
# The std build reuses bootstrap's own build across compilers that print the same `rustc -vV`

`src/sysroot.rs`'s `build_std` empties `stage0-std/<target>` but keeps `<fork>/build/toyos-std/bootstrap`, which bootstrap.py compiles with `build.rustc`. Every ToyOS compiler prints the same `rustc -vV` (`issues/build/two-toolchain-releases-report-the-same-rustc-vv.md`), so after the compiler a checkout names changes, cargo keeps every dep as Fresh, and the next `src/bootstrap` change recompiles bootstrap with the new compiler against rlibs whose `std` it does not have: `error[E0463]: can't find crate for serde` (33 errors). Recorded in toyos-desk1 at 70156f1b (primary stage2 → compilers/a04a68b92a50e478). The primary's own `build/toyos-std/bootstrap` is already rejected by its stage2 (E0460).
**Owner**: the next build-tooling brief.
**Exit**: `build_std` removes `build/toyos-std/bootstrap` when `Compiler::identity()` differs from the one it last recorded, shown by a sysroot.rs test in which a changed identity removes it; or the description fix of the linked issue lands for every compiler this tree builds.
