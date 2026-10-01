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

**Exit**: `SYS_SYMLINK` over a name that exists is refused `AlreadyExists`
under the lock that makes the link, and a listing of `/tmp` names each link it
holds, each asserted by a guest case. libc's `symlink` calls it with nothing
asked first and answers that refusal `EEXIST`, which is how LLVM's
`LockFileManager` takes its lock (`create_link`, `::symlink` in
`llvm/lib/Support/Unix/Path.inc`).
