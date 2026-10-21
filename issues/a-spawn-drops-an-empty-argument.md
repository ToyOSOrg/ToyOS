---
status: open
kind: defect
opened: 2026-10-20
---

# A spawn drops an empty argument and refuses one that is not UTF-8

`SYS_SPAWN` reads `argv` as one `NUL`-separated string with `user_str`, which
refuses bytes that are not UTF-8 (`InvalidArgument`), and splits it with
`text.split('\0').filter(|s| !s.is_empty())` (`kernel/src/syscall/dispatch.rs`).
So a child is never handed an empty argument, and a program asked to run
`cmd "" x` sees `cmd x`; and an argument C or std carries as bytes, which POSIX
and `OsStr` allow, refuses the spawn. Read from the code while building
`posix_spawn` (stage 3 of
`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`), not
reproduced: `tests/guest/libc` passes `""` only where the child reads no
argument. Build tools pass empty arguments (`make`'s recipes, `sh -c ''`).

*Exit*: a child spawned with `argv` `["a", "", "b"]` and with a non-UTF-8
argument reads both as given, in a guest test, red with the filter put back.
The blob needs a representation that says where an argument ends without
leaning on an empty one, an argument count beside it, say.
