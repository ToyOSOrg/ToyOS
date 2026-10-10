---
status: open
kind: defect
opened: 2026-10-09
---

# `remove_all` follows a link swapped in mid-walk

Every C++ program on ToyOS gets libc++'s `std::filesystem::remove_all` built
with `-DREMOVE_ALL_USE_DIRECTORY_ITERATOR` (`src/libcxx.rs`): it walks a
directory by its path, and a directory swapped for a symbolic link between the
walk's check and its descent sends the deletes wherever the link points,
outside the tree it was asked to remove. libc++ says so of that walk
(`libcxx/src/filesystem/operations.cpp`, "vulnerable to some race conditions",
https://reviews.llvm.org/D118134), the class of CVE-2022-21658. Its other walk
holds a descriptor for each directory and resolves every name against it
(`openat`, `fdopendir`, `unlinkat`, `O_DIRECTORY`, `O_NOFOLLOW`,
`AT_REMOVEDIR`), and ToyOS has no such descriptor: `open` refuses every
directory, and the kernel resolves every path from the root.

Owner: `issues/toyos-builds-itself.md`, M2, whose clang brought
`std::filesystem` in.

**Exit**: a directory opens as a handle that the kernel resolves names
against, libc defines `openat`, `fdopendir` and `unlinkat` over it, and
`src/libcxx.rs` passes no `REMOVE_ALL_USE_DIRECTORY_ITERATOR`.
