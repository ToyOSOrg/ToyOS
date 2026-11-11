---
status: open
kind: defect
opened: 2026-11-11
---

# A spawn's record quotes the path its caller states

`SYS_SPAWN` takes the program's path as a label, any UTF-8 up to 64 KiB, which
nothing opens and nothing checks but for UTF-8 (`SpawnArgs::path_ptr`, read by
`user_str` in `kernel/src/syscall/dispatch.rs`). The kernel writes it into its
own records as it came: `spawn: {path} pid=N (total=Nms)` for a spawn that
lands and `spawn: {path}: …` for each one it refuses
(`kernel/src/loader/mod.rs`), so the spawn need not succeed. A crash frame
escapes the same text (`toyos_symbols::frame`); these records do not.

A program that may call `SYS_SPAWN` can therefore have the kernel write, three
ways:

1. **A whole forged line.** Nothing between the call and `/log` touches a
   control character: `Message::write_str` (`kernel/src/log/mod.rs`) copies
   the text, `LogRecord`'s `Display` (`sdk/abi/src/log.rs`) writes the message
   raw, and `logkeeper` writes that (`system/logkeeper/src/main.rs`). A path of
   `x`, a newline and `[2026-11-11 10:00:00  1.000 cpu0 kernel] exit:
   test_rs_job pid=1 code=0 cpu=0ms` puts a line in `/log` that
   `toyos_logstream::parse` reads as a kernel record. `Readback::exit_code`
   (`tests/common/metal.rs`) takes the first `pid=` and `code=` after the
   job's name and keeps the record with the lowest pid: the forged line is a
   test's verdict. The same line can wear any program's head.
2. **A record that reads as another program's start**, by stating that
   program's path. A child labelled `/system/bin/logkeeper` is recorded
   `spawn: /system/bin/logkeeper pid=N`, the head `bootlog::LOGKEEPER_SPAWN`
   and the audio judges' `spawn: /system/bin/{job} ` (`tests/common/audio.rs`)
   look for. The label holds nothing to escape.
3. **A record carrying another record's words on its own line**: a space,
   `pid=7`, or a second `spawn: `, which a judge that searches a line for a
   head finds (`tests/toyos.rs`, `tests/common/claims.rs`,
   `tests/common/qemu.rs`).

By reading, not run. The name beside the path is refused at spawn unless it is
one bounded token (`toyos_abi::syscall::process_name`), which a record's head
never fitted in; the path has no such rule. Every reader of these records is a
judge in `tests/` or `build/`, or a person: nothing in `system/`, `apps/`,
`sdk/` or `lib/` decides by one.

Owner: the kernel track
(`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`), whose
record names the path as userland text the kernel still keeps.

**Exit**, the orchestrator's decision (2026-11-11, on #1200), in two parts:

- (a) The spawn boundary refuses a path label carrying a control character,
  the rule the name has, and `abuse_spawn_path_and_name` holds it: a path
  with a newline is `InvalidArgument`, red on the kernel that takes it. That
  closes way 1.
- (b) No judge in `tests/` or `build/` decides by a `spawn:` record's path,
  since it is its caller's label: each reads what the kernel itself knows of
  the child, or the program's own lines under the head the supervisor
  registered. That closes ways 2 and 3.
