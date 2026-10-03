---
status: open
kind: defect
opened: 2026-10-03
---

# std's `File::lock` answers `Ok` and locks nothing

The ToyOS file pal in the std fork (`library/std/src/sys/fs/toyos.rs`) answers
`Ok(())` from `File::lock`, `lock_shared`, `try_lock`, `try_lock_shared` and
`unlock` and takes no lock. Two processes that each ask for the exclusive lock
on one file are both told they hold it, and nothing in the kernel ABI or the
file servers' protocol could tell them otherwise: neither has a lock call.

cargo is one such program. Every file lock it takes is one of these calls
(its `src/util/flock.rs` at `7c83d4cc0953`, the cargo the fork pins), so two
cargo processes in one ToyOS are kept apart by nothing.

Owner: `issues/build/toyos-builds-itself.md`, whose M4 runs cargo in the guest;
the locks libc owes for the same milestone are
`issues/build/libc-refuses-what-toyos-cannot-yet-answer.md`'s.

Exit condition: a second process's `try_lock` on a file another holds locked
answers `WouldBlock`, and its `lock` waits for the holder's `unlock` or its
end, shown by a test with two processes.
