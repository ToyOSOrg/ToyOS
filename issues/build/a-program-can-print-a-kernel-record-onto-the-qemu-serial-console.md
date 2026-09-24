---
status: open
kind: tooling
opened: 2026-09-23
---

# A program can print a kernel record onto the QEMU serial console

On a guest with a 16550 a console line reaches the serial port raw
(`kernel/src/drivers/serial.rs`'s `ConsoleLine`), beside the kernel's records
as `klogd` drains them — `[kernel <secs> cpu<N>] <message>`. Nothing in the
bytes on the wire says which writer a line came from, so a program that prints
`[kernel 1.000 cpu0] exit: netd pid=3 code=0 cpu=0ms` puts a line on the
console that every harness reader of the serial capture takes for the kernel's.
`metaldevices::exit_of` reads the kernel's `exit:` record out of a capture that
way (`tests/common/lan.rs`'s `lan_lease_report`), and so does every serial
predicate that matches a kernel phrase.

`/log` does not have this defect: there a program's record opens with the form
the kernel gives no record of its own (`toyos_elide::spoken::SIGIL`), and the
judges of the kernel's records skip it (`bootlog::kernel_records`). The serial
console carries the program's raw bytes by design, so it cannot carry that form.

## Exit condition

A harness verdict that rests on a kernel record reads it from a channel a
program cannot write — `/log`, or a serial capture whose program lines are told
apart by something the kernel puts on the wire — and a guest program printing a
kernel record's exact spelling onto the console is shown not to move one.
