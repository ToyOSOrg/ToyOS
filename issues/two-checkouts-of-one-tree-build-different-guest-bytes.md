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

- The kernel carries no path into a checkout and differs, on both
  architectures, in nothing `strings` shows but local symbols' `.llvm.<n>`
  suffixes, which LLVM derives from the module's identity: 2362 symbols on
  x86_64 and 2093 on AArch64.
- `snake` carries no path into the checkout that built it, and still differs
  the same way: 203 symbols.
- The bootloader is byte-identical at two paths, on both architectures. It
  and the kernel carried each path dependency's absolute source path in their
  panic locations while each was its own workspace root; in one workspace
  cargo hands rustc every path package's source relative to the root.
- A workspace of its own still does that. 45 of the 100 `test_rs_*` binaries
  and `libtls_cranelift.so` on the `testcases` ROOT carry the building
  checkout's absolute path: `toyos`'s and `toyos-abi`'s panic locations, and
  cranelift-codegen's generated sources under the cdylib's own target
  directory.
- Every guest binary carries the path of the checkout that built the sysroot
  store's std and C library, not the one building: chiefly the panic
  locations of their path dependencies `toyos`, `toyos-abi` and
  `toyos-osrelease`. A worktree whose sysroot came from the store carries
  another checkout's path. On the `testcases` ROOT that is all 250 ELF
  files (23 in each C case, 6 in `fileserver`), and on the `cargo run` ROOT
  all 27 programs (6 in `snake`).
- Every guest binary, the kernel and the bootloader carry the cargo home's
  absolute path: the panic locations of registry and git dependencies (36 in
  `snake`, 7 in the kernel, 40 in the bootloader). Two checkouts on one host
  share it; two hosts do not.

`LC_ALL=C grep -a -o "$HOME/" <binary> | wc -l` counts a binary's paths under
the home directory; `snake` gives 42, its 6 and 36.

Nothing remaps either: no `--remap-path-prefix`, no `trim-paths`. Until both
are path-independent a Linux build and a macOS build cannot be compared either,
since their checkouts never share a path.

Exit: two checkouts at different paths build byte-identical guest artifacts,
and a gate builds two and compares them.
