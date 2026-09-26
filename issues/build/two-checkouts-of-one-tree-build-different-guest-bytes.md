---
status: open
kind: defect
opened: 2026-09-26
---

# Two checkouts of one tree build different guest bytes

Self-hosting is ToyOS rebuilding itself and reproducing the host's bytes, and
the host does not yet reproduce its own across checkout paths. The same
sources copied to two paths and built with the same sysroot, profile and
linker gave three different kernels, bootloaders and `snake`s; the same path
built twice into two target directories gave identical ones.

- The kernel and the bootloader carry each path dependency's absolute source
  path in their panic locations (55 strings in the kernel, 13 in the
  bootloader, measured with `strings`).
- `snake` carries no path, and still differs: 203 local symbols' `.llvm.<n>`
  suffixes, which LLVM derives from the module's identity.

Nothing remaps either: no `--remap-path-prefix`, no `trim-paths`. Until both
are path-independent a Linux build and a macOS build cannot be compared either,
since their checkouts never share a path.

Exit: two checkouts at different paths build byte-identical guest artifacts,
and a gate builds two and compares them.
