---
status: open
kind: defect
opened: 2026-10-22
---

# A spawned C child inherits no ignored signal and no mask

POSIX has a child of `posix_spawn` start with the signals its parent ignores
still ignored and its parent's mask, unless the attributes say otherwise (XSH
`posix_spawn`, `POSIX_SPAWN_SETSIGDEF` and `POSIX_SPAWN_SETSIGMASK`). libc's
children start with every signal at its default and none blocked
(`sdk/libc/src/spawn.rs`), since nothing carries a disposition or a mask
across `SYS_SPAWN`. `SETSIGDEF` is therefore always true and a `SETSIGMASK`
naming a signal is refused `ENOSYS`, but a parent ignoring `SIGINT` or
`SIGHUP` starts a child a quit still ends, which is what a shell's background
job and `nohup` rely on not happening.

Carrying them needs the child to listen to its notice from its first
instruction, before its own libc runs, or a quit in that window kills it: a
decision of how a spawn hands a child its signal state, which nobody has
made.

Read from the code, not measured.

**Exit**: a C child spawned while its parent ignores `SIGINT` runs on after an
interrupt; one spawned with `SIGINT` in its caller's mask, or in
`SETSIGMASK`, shows it in `sigpending` and ends 130 on unblocking it; and one
whose `SETSIGDEF` names it is ended by an interrupt; in `libc_children`'s boot.

**Owner**: `sdk/libc`, with the spawn it starts a child by.
