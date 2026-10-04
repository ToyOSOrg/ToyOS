---
status: open
kind: defect
opened: 2026-09-30
---

# libc++ for ToyOS is built without `std::filesystem`

`src/libcxx.rs` configures the C++ runtime with `LIBCXX_ENABLE_FILESYSTEM=OFF`,
so a ToyOS C++ program that includes `<filesystem>` does not compile, and
`<fstream>` goes with the option: `std::ifstream` and `std::ofstream` are
undefined templates. Built with it, libc++ asks libc for `setbuf`, `fseeko`,
`ftello`, `utimes`, `truncate`, `pathconf`, `openat`, `unlinkat`, `fdopendir`,
`_PC_PATH_MAX`, `O_DIRECTORY`, `O_NOFOLLOW`, `AT_FDCWD` and `AT_REMOVEDIR`.

**Exit**: libc carries what libc++'s `src/filesystem` calls, the option goes,
and a guest test lists a directory through `std::filesystem`.
