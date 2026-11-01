---
status: open
kind: defect
opened: 2026-10-23
---

# A relative program spawned as a helper is passed on unresolved

std's `Command` makes an independent launch's program absolute against the
child's working directory (`toyos::launch::program_path`), but a helper's
program it passes on as the caller wrote it: `resolve_program`
(`sdk/std/sys/process.rs`) returns any path holding a `/` unchanged, and that
path is what the image is read from on a file server and what the kernel's
loader opens. libc's `posix_spawn` and toybox's `stats` pass theirs the same
way. The shell is the one caller that makes what its user typed absolute
itself (`build_command`, `system/shell/src/main.rs`), for every command.

So `stats ./foo`, and a C or Rust program spawning `./foo`, may be refused
where the shell's `./foo` starts. Read from the code, not reproduced: no
guest test spawns a relative path as a helper.

Exit: a helper's relative program is made absolute against the child's
working directory by `toyos::spawn`, the one rule std and libc share, and the
shell's own call goes; a guest probe spawns `./foo` from a working directory by
std, by libc and through `stats`, and reads each child's output back.
