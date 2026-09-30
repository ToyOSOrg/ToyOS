---
status: open
kind: defect
opened: 2026-09-30
---

# libc's fcntl and fchmod answer 0 and do nothing

`fcntl` (`userland/libc/src/posix_io.rs`) answers 0 to every command on every
descriptor: `F_DUPFD` names descriptor 0 as the duplicate, `F_GETFL` reads
every descriptor as read-only and blocking, `F_SETFL` sets nothing, and a
command it does not know is answered as done. `fchmod` answers 0 and changes
nothing. Once `fcntl.h` declares the record locks
(`issues/build/toyos-libc-lacks-the-posix-surface-llvm-compiles-against.md`),
LLVM's `sys::fs::tryLockFile` and `lockFile`
(`llvm/lib/Support/Unix/Path.inc`) take a lock nothing holds. Close-on-exec,
`F_GETFD` and `F_SETFD`, is the descriptor table's of stage 3 of
`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`.

**Exit**: every other command POSIX defines for `fcntl` does what POSIX says or
answers -1 with `errno` set — a record lock `EINVAL`, as POSIX has a file that
supports no locking answer — and a command POSIX does not define answers
`EINVAL`; `fchmod` changes the mode `fstat` reads back, or answers -1 with
`errno` set. A guest C case asserts each command's answer and `fchmod`'s.
