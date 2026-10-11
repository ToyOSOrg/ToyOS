---
status: open
kind: defect
opened: 2026-10-03
---

# A ToyOS-hosted LLVM is configured as running on the build machine

LLVM takes `LLVM_HOST_TRIPLE` from `config.guess`, run by the CMake that
configures it (`llvm/cmake/modules/GetHostTriple.cmake`,
`llvm/cmake/config-ix.cmake`), unless the configuration names one. The
ToyOS-hosted clang and LLD name it (`src/hostedclang.rs`), and their
configure logs `LLVM host triple: x86_64-unknown-toyos`. Bootstrap names none:
the `rust/` fork's `src/bootstrap/src/core/build_steps/llvm.rs` defines
`LLVM_DEFAULT_TARGET_TRIPLE` and no `LLVM_HOST_TRIPLE`. So the LLVM that M3's
rustc carries, built by bootstrap for a ToyOS host
(`issues/rustc-llvm-cannot-build-for-a-toyos-host.md`), records the machine
that built it as its host. The configure of that build on the development
Mac, the fork at the commit `main` pinned then (`95960d6c214`), logged (#682,
comment 5968053849)

```
-- LLVM host triple: arm64-apple-darwin27.0.0
-- LLVM default target triple: x86_64-unknown-toyos
```

`config.guess` knows no ToyOS, and `get_host_triple` ends the configure where
it exits non-zero, so a configure run inside ToyOS stops there. That half is
read from `GetHostTriple.cmake`, not run.

Owner: `issues/toyos-builds-itself.md`: M3 for the triple bootstrap's build
records, M5 for the configure inside ToyOS.

Exit condition: bootstrap's configure of a ToyOS-hosted LLVM logs `LLVM host
triple: x86_64-unknown-toyos`, and a configure inside ToyOS reaches its end.
