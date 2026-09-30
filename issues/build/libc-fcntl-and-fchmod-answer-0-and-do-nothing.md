---
status: open
kind: defect
opened: 2026-09-30
---

# libc's fcntl, chmod and fchmod answer 0 and do nothing

`fcntl` (`userland/libc/src/posix_io.rs`) answers 0 to every command on every
descriptor: `F_DUPFD` names descriptor 0 as the duplicate, `F_GETFL` reads
every descriptor as read-only and blocking, `F_SETFL` sets nothing, and a
command it does not know is answered as done. `chmod` and `fchmod` answer 0 and
change nothing, and LLVM's `sys::fs::setPermissions` calls each
(`llvm/lib/Support/Unix/Path.inc`). Once `fcntl.h` declares the record locks
(`issues/build/toyos-libc-lacks-the-posix-surface-llvm-compiles-against.md`),
LLVM's `sys::fs::tryLockFile` and `lockFile`, in the same file, take a lock
nothing holds. Close-on-exec, `F_GETFD` and `F_SETFD`, is the descriptor
table's of stage 3 of
`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`.

**Exit**: every other command POSIX defines for `fcntl` does what POSIX says or
answers -1 with `errno` set — a record lock `EINVAL`, as POSIX has a file that
supports no locking answer — and a command POSIX does not define answers
`EINVAL`; `chmod` and `fchmod` each change the mode `stat` and `fstat` read
back, or answer -1 with `errno` set. A guest C case asserts each command's
answer: `F_GETFL` on a descriptor opened `O_RDWR | O_APPEND` answers both, and
`F_DUPFD` a descriptor no lower than its argument. It asserts what `chmod` and
`fchmod` answer, and the mode `stat` and `fstat` then read.
