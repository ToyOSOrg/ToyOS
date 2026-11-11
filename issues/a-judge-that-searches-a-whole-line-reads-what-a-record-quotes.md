---
status: open
kind: finding
opened: 2026-11-11
---

# A judge that searches a whole line reads what a record quotes

A spawn's record quotes the path its caller stated, any text of one line
(`toyos_abi::syscall::spawn_path`); a crash's frames escape it, spaces among
what they escape (`toyos_symbols::frame`). `Serial::must_say`, `must_not_say` and the waits on
a marker (`tests/common/serial.rs`, `tests/common/qemu.rs`) ask whether a line
contains their words, so a record that quotes those words answers them: a
program that may call `SYS_SPAWN` can have the kernel write `spawn: pid=N
(total=Nms) <a program's line, or another record's message>`.

No judge reads a spawn's record for its path any more: one is found by the
pid at its head (`bootlog::spawned_pid`). The judges that match a kernel
record at its head (`bootlog::message`) or a program's line under its head
(`toyos_logstream::program_line`) are not answered by a quotation either.

Evidence: by reading. Every program on a test boot is the suite's own, so no
run has shown it.

Exit: a judge's words are matched against a kernel record's message from its
head, or a program's line under the head the supervisor registered, never
against a whole line. Owner: the harness.
