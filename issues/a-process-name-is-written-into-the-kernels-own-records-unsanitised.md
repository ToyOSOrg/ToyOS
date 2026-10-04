---
status: open
kind: defect
opened: 2026-09-23
---

# A process name is written into the kernel's own records unsanitised

`make_name` (`kernel/src/loader/start.rs`) keeps a spawned file's name as the
process-table name, and the kernel writes that name into records a judge parses
by field: `exit: {name} pid=N code=N cpu=Nms` (`kernel/src/process.rs`) is the
verdict channel `bootlog::EXIT` and `metaldevices::exit_of` read. A program
that can write and spawn a file whose name holds a space and the record's own
words — `test_rs_job pid=1 code=0`, inside `THREAD_NAME_LEN` — gets a kernel
record that opens as the job's own: `exit: test_rs_job pid=1 code=0 pid=7
code=3 cpu=0ms`. `exit_of(log, "test_rs_job")` matches its head, takes the
first `code=` after it, and the forger's record is the last one. Not run: the
reading of the two functions.

A program's own lines are not this: they reach `/log` under the head `logd`
writes for them, whose name is init's and refused outside `[A-Za-z0-9._+-]`
(`toyos_logstream::Tag`). This is the kernel's record quoting a name it does
not clean.

## Exit condition

A name the kernel writes into a record it or a judge parses by field cannot
carry that record's separators — sanitised where it is written, or refused at
spawn — and a test that spawns a file named after a record's fields reads the
real exit code out of `/log`.
