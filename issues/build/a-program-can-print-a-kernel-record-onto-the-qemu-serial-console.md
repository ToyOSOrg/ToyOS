---
status: open
kind: tooling
opened: 2026-09-23
---

# A program can print a kernel record onto the QEMU serial console

On a guest with a 16550 two writers share the wire: `klogd` drains the
kernel's records as `[kernel <secs> cpu<N>] <message>`, and `logd` puts each
program's line on its own console as the program wrote it
(`userland/logd/src/main.rs`). Nothing in the bytes on the wire says which
writer a line came from, so a program that prints
`[kernel 1.000 cpu0] exit: netd pid=3 code=0 cpu=0ms` puts a line on the
console that every harness reader of the serial capture takes for the kernel's.
`metaldevices::exit_of` reads the kernel's `exit:` record out of a capture that
way (`tests/common/lan.rs`'s `lan_lease_report`), and so does every serial
predicate that matches a kernel phrase.

`/log` and the log `logd` serves do not have this defect: there a program's
line opens with the head `logd` writes for it (`toyos_logstream::ProgramLine`),
and the judges of the kernel's records leave it out (`bootlog::kernel_records`).
The wire carries a program's line bare so the harness reads it as it always
has.

## Exit condition

A harness verdict that rests on a kernel record reads it from a channel a
program cannot write — `/log`, the served log, or a serial capture whose
program lines carry the head `logd` gives them — and a guest program printing a
kernel record's exact spelling onto the console is shown not to move one.
