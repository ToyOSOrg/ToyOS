---
status: open
kind: defect
opened: 2026-09-26
---

# Two checkouts of one tree build different guest bytes

Self-hosting is ToyOS rebuilding itself and reproducing the host's bytes, and
the host does not yet reproduce its own across checkout paths. The same
sources copied to two paths and built with the same sysroot, profile and
linker gave three different kernels and `snake`s; the same path built twice
into two target directories gave identical ones. The LLVM is held
fixed here: two builds of one LLVM key differing is
`issues/two-builds-of-one-llvm-key-differ-in-their-bytes.md`.

- The kernel carries no path and differs, on both architectures, in nothing
  `strings` shows but local symbols' `.llvm.<n>` suffixes, which LLVM derives
  from the module's identity: 2362 symbols on x86_64 and 2093 on AArch64.
- `snake` carries no path, and still differs the same way: 203 symbols.
- The bootloader is byte-identical at two paths, on both architectures. It
  and the kernel carried each path dependency's absolute source path in their
  panic locations while each was its own workspace root; in one workspace
  cargo hands rustc every path package's source relative to the root.

Nothing remaps either: no `--remap-path-prefix`, no `trim-paths`. Until both
are path-independent a Linux build and a macOS build cannot be compared either,
since their checkouts never share a path.

Exit: two checkouts at different paths build byte-identical guest artifacts,
and a gate builds two and compares them.
