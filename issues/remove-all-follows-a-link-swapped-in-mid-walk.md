---
status: open
kind: defect
opened: 2026-10-09
---

# `remove_all` follows a link swapped in mid-walk

Every C++ program on ToyOS gets libc++'s `std::filesystem::remove_all` built
with `-DREMOVE_ALL_USE_DIRECTORY_ITERATOR` (`build/libcxx.rs`): it walks a
directory by its path, and a directory swapped for a symbolic link between the
walk's check and its descent sends the deletes wherever the link points,
outside the tree it was asked to remove. libc++ says so of that walk
(`libcxx/src/filesystem/operations.cpp`, "vulnerable to some race conditions",
https://reviews.llvm.org/D118134), the class of CVE-2022-21658. Its other walk
holds a descriptor for each directory and resolves every name against it
(`openat`, `fdopendir`, `unlinkat`, `O_DIRECTORY`, `O_NOFOLLOW`,
`AT_REMOVEDIR`). libc has each of those but `O_NOFOLLOW`, and a descriptor of
a directory the kernel's `/tmp` holds names its path, which the kernel
resolves from the root (`sdk/toyos/src/files.rs`).

Owner: `issues/toyos-builds-itself.md`, beside its path, since no exit there
needs it; its M2 clang brought `std::filesystem` in.

**Exit**: libc's `openat` takes `O_NOFOLLOW`, a descriptor of every
directory names the directory itself and not its path, and
`build/libcxx.rs` passes no `REMOVE_ALL_USE_DIRECTORY_ITERATOR`.
