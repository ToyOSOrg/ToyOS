---
status: open
kind: defect
opened: 2026-10-20
---

# A spawn drops an empty argument

A spawn's `argv` is one `NUL`-separated run of bytes (`toyos::spawn::Spawn`),
which the startup block's writer splits skipping every empty string
(`toyos::startup::bytes`), as the kernel did while it copied `argv`. So a
child is never handed an empty argument, and a program asked to run
`cmd "" x` sees `cmd x`. An argument that is not UTF-8 reaches the block and a
C child as given, and a Rust child's `std::env::args_os` as an empty one
(`sdk/std/sys/args.rs`). Read from the code while building
`posix_spawn` (stage 3 of
`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`), not
reproduced: `tests/guest/libc` passes `""` only where the child reads no
argument. Build tools pass empty arguments (`make`'s recipes, `sh -c ''`).

A launch through the supervisor carries the same blob (`toyos::launch`) and
loses the same arguments before any spawn: `serve_launch` (`system/supervisor/src/main.rs`)
splits `request.argv` on `NUL`, filters out every empty argument, and skips an
argument that is not UTF-8 without refusing the launch, so a launched child
sees neither.

*Exit*: a child spawned with `argv` `["a", "", "b"]` and with a non-UTF-8
argument reads both as given, in a guest test, red with the writer's filter put
back; and a host test of the supervisor's decode of a launch request's `argv`
keeps an empty and a non-UTF-8 argument, red with its filter put back.
The blob needs a representation that says where an argument ends without
leaning on an empty one, an argument count beside it, say.
