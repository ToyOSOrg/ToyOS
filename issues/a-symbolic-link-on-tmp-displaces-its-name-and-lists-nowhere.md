---
status: open
kind: defect
opened: 2026-10-01
---

# A symbolic link on /tmp displaces its name and lists nowhere

Read from the code, not run. The kernel's `/tmp` (`kernel/src/tmpfs.rs`)
makes a link over whatever file its name held, where POSIX's `symlink` refuses
a name that exists `EEXIST`, so libc's `symlink` refuses `ENOSYS` for a link
the kernel would hold (`sdk/libc/src/posix_io.rs`). Its `list` answers files
alone, so no `readdir` of a directory shows a link it holds.

libc's `symlink` already sends a link a file server is to hold to that
server, through `toyos::files` as std's does, which refuses a name that
exists, answered `EEXIST`, and `readdir` there names the link; corpus case
`211_home_files` asserts both under `/home`. What is left is `/tmp`.

**Exit**: `/tmp` is a file server's, so every `symlink` is one, sent with
nothing asked first and answered `EEXIST` for a name that exists, and libc's
`readdir` of `/tmp` names the link, each asserted by a test. The kernel's `SYS_SYMLINK`
and its symlink code (`kernel/src/vfs.rs`, `kernel/src/tmpfs.rs`) go wherever
nothing calls them. That `EEXIST` is how LLVM's `LockFileManager` takes its
lock (`create_link`, `::symlink` in `llvm/lib/Support/Unix/Path.inc`). An LLVM
build on ToyOS reaches `symlink` too:
`LLVM_USE_SYMLINKS` is on for a UNIX host, so `add_llvm_tool_symlink`
(`llvm/cmake/modules/AddLLVM.cmake`) makes each tool's aliases with CMake's
`create_symlink`.
