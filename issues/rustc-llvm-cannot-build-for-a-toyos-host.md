---
status: open
kind: defect
opened: 2026-09-30
---

# rustc_llvm cannot build for a ToyOS host

M3's rustc (`issues/toyos-builds-itself.md`) carries LLVM through
`compiler/rustc_llvm`, whose `build.rs`, read from the fork at `rust/`, not
run, builds it for `x86_64-unknown-toyos` wrongly three ways:

- It links `stdc++` for a target it does not list unless bootstrap's
  `use-libcxx` asks for `c++`, and ToyOS's one C++ runtime is `libc++`.
- It links no C library: a cross build asks `llvm-config` for no system
  libraries.
- It finds a cross target's LLVM by replacing the host triple in the host
  `llvm-config`'s paths, and the store's paths (`src/llvm.rs`) do not contain
  it, so the replacement names the host's LLVM.

The C++ runtime and the C library are ToyOS arms at existing dispatch sites,
written as upstream would take them. The paths are no arm, and rustc does not
change for them: ToyOS's build is to hand a hosted rustc's build a host LLVM whose
paths hold the host triple where the ToyOS host's LLVM's hold
`x86_64-unknown-toyos`, as bootstrap's own build directory lays the two out.

**Exit**: bootstrap builds `rustc_llvm` for `x86_64-unknown-toyos`, and the
guest's `rustc -vV` prints `LLVM version:`.
