---
status: open
kind: defect
opened: 2026-10-04
---

# A supervisor panic before logkeeper starts reaches no console

The supervisor makes its own lines records in its log ring first thing
(`Log::open` in `system/supervisor/src/main.rs`), and only `logkeeper`
drains a ring. So a panic before `logkeeper` runs leaves its message in a
ring nobody reads.

The bootstrap's own refusals are said on the console first
(`bootstrap_fails`): the SYSTEM file server not starting, `/system/etc/os-release`
or `/system/etc/system.manifest` not read, and an os-release the build did not
write. What is left: a manifest `toyos_manifest::parse` panics on (a build
whose two halves disagree), and every other panic between `Log::open` and
`logkeeper`'s start.

Measured before `bootstrap_fails`, with ROOT carrying no os-release
(`machine_shutdown`, x86-64, QEMU): the console's last kernel record is
`exit: supervisor pid=0 code=101`, the panic's words appear nowhere, and the
harness waits out its 63 s boot ceiling for `===READY===`. It does not treat
the supervisor's end as a death.

**Exit**: a supervisor panic before `logkeeper` serves puts its message on the
console, and the harness ends a boot whose supervisor exited, naming the exit.
