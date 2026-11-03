---
status: open
kind: defect
opened: 2026-11-04
---

# A supervisor that fails before logkeeper runs says why nowhere

The supervisor's boot ends by name when `/system`'s file server does not
start, or `/system` cannot be read (`bootstrap_fails`,
`system/supervisor/src/main.rs`). Until the kernel's console thread was
deleted it wrote that line on the kernel console the kernel spawned it with,
which the thread put on the UART. That console is gone with the thread: the
UART is its userland driver's, which `logkeeper` feeds, and `logkeeper` is a
program on `/system`. The line now goes to the supervisor's own ring, which
nothing reads before `logkeeper` runs, and then the supervisor panics.

What is left is the kernel's: the supervisor's `exit:` record, and the
deadline's report, which put both on the UART and in the black box. Neither
says why.

**Evidence:** the code; no test stages a broken `/system`.

**Owner:** the supervisor's.

**Exit:** a boot whose `/system` will not serve says why on a channel an
operator reads — the black box, or the UART at the fatal path that follows —
shown by a boot staged with a `/system` that does not serve.
