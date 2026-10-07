---
status: open
kind: defect
opened: 2026-10-01
---

# A symbolic link on /tmp displaces its name and lists nowhere

Read from the code, not run. The kernel's `/tmp` (`kernel/src/tmpfs.rs`)
makes a link over whatever file its name held, where POSIX's `symlink` refuses
a name that exists `EEXIST`, so libc's `symlink` refuses `ENOSYS`
(`userland/libc/src/refused.rs`). Its `list` answers files alone, so no
`readdir` of a directory shows a link it holds.

**Exit**: `symlink` is the file server's, not the kernel's. libc's `symlink`
sends the request to the server of the directory that is to hold the link, as
std's does (`on_path` in `sdk/std/sys/fs.rs`), with nothing
asked first, and answers its `AlreadyExists` `EEXIST`. fsd makes the link in
the one request that refuses a name that exists, and libc's `readdir` of that
directory names the link, each asserted by a test. The kernel's `SYS_SYMLINK`
and its symlink code (`kernel/src/vfs.rs`, `kernel/src/tmpfs.rs`) go wherever
nothing calls them. That `EEXIST` is how LLVM's `LockFileManager` takes its
lock (`create_link`, `::symlink` in `llvm/lib/Support/Unix/Path.inc`). An LLVM
build on ToyOS reaches `symlink` too:
`LLVM_USE_SYMLINKS` is on for a UNIX host, so `add_llvm_tool_symlink`
(`llvm/cmake/modules/AddLLVM.cmake`) makes each tool's aliases with CMake's
`create_symlink`.
