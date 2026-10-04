---
status: open
kind: defect
opened: 2026-10-04
---

# A supervisor panic before logkeeper starts reaches no console

`/system/bin/supervisor` makes its own lines records in its log ring first thing
(`Log::open` in `userland/supervisor/src/main.rs`), and only `logkeeper`
drains a ring. So a panic before `logkeeper` runs leaves its message in a
ring nobody reads. The refusals of a ROOT without
`/system/etc/os-release`, or without a readable `/system/etc/system.manifest`,
are such panics.

Measured with ROOT carrying no os-release (`machine_shutdown`, x86-64, QEMU):
the console's last kernel record is `exit: supervisor pid=0 code=101`, the
panic's words appear nowhere, and the harness waits out its 63 s boot ceiling
for `===READY===`. It does not treat the supervisor's end as a death.

**Exit**: a supervisor panic before `logkeeper` serves puts its message on the
console, and the harness ends a boot whose supervisor exited, naming the exit.
