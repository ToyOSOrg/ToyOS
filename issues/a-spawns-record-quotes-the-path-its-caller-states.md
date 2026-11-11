---
status: open
kind: defect
opened: 2026-11-11
---

# A spawn's record quotes the path its caller states

`SYS_SPAWN` takes the program's path as a label, any UTF-8 up to 64 KiB, which
nothing opens and nothing checks (`SpawnArgs::path_ptr`, read by `user_str` in
`kernel/src/syscall/dispatch.rs`), and the kernel writes it into its own
record as it came: `spawn: {path} pid=N (total=Nms)` and each refusal's
`spawn: {path}: …` (`kernel/src/loader/mod.rs`). A crash frame escapes the
same text (`toyos_symbols::frame`); the spawn record does not.

So a program that may spawn can have the kernel write a record that reads as
another program's start, two ways:

- by stating another program's path. A child labelled
  `/system/bin/logkeeper` is recorded `spawn: /system/bin/logkeeper pid=N`,
  the head `bootlog::LOGKEEPER_SPAWN` and the audio judges' `spawn:
  /system/bin/{job} ` (`tests/common/audio.rs`) look for;
- by stating a path that holds the record's own words: a space, `pid=7`, or a
  second `spawn: `, which a judge that searches a line for a head finds
  (`tests/toyos.rs`, `tests/common/claims.rs`, `tests/common/qemu.rs`).

By reading, not run. The name beside the path is refused at spawn unless it is
one bounded token (`toyos_abi::syscall::process_name`); the path has no such
rule.

Owner: the kernel track
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`), whose
record names the path as userland text the kernel still keeps.

**Exit**: the kernel's spawn record cannot be read as another process's:
either the record names the child by what the kernel itself knows of it (its
pid and its checked name) and the path leaves the kernel, or the path is
escaped where it is written as a crash frame's is. A guest test that spawns a
child labelled `/system/bin/logkeeper pid=1` reads one record in `/log`, and
no judge takes it for logkeeper's; red on the kernel that writes the label as
it came.
