---
status: open
kind: defect
opened: 2026-09-30
---

# libc++ for ToyOS is built without `std::filesystem`

`src/libcxx.rs` configures the C++ runtime with `LIBCXX_ENABLE_FILESYSTEM=OFF`,
so a ToyOS C++ program that includes `<filesystem>` does not compile. libc++'s
filesystem is written on the POSIX directory, `stat` and path calls, and libc
has no `dirent.h`.

**Exit**: libc carries what libc++'s `src/filesystem` calls, the option goes,
and a guest test lists a directory through `std::filesystem`.
