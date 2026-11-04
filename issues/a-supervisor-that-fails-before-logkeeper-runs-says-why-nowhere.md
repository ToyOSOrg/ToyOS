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
UART is its userland driver's, which `logkeeper` feeds, and both are
programs on `/system`, which is what failed. The line goes to the
supervisor's own ring, which nothing reads, and the supervisor panics.

What is left is the kernel's records: the supervisor's `exit:` and the file
server's, which no channel carries either, because the kernel's own writes
to the UART after the hand-over are the stop's and a fatal path's alone. A
boot armed with a deadline ends in its report, which puts them on the UART
and in the black box; a shipping image, armed with none, idles with nothing
said. Neither says why.

Holding the kernel's inline drain past the hand-over would not carry the
line, which is not a kernel record, and would put every record's UART write
back on its producer — an interrupt handler or a lock holder — until the
driver runs.

**Evidence:** the code; no test stages a broken `/system`.

**Owner:** the supervisor's, with the kernel's fatal path; the shape is the
owner's to rule (`#1165`).

**Exit:** a boot whose `/system` will not serve says why, and ends, on a
channel an operator reads on every machine — the UART where there is one, the
panel and the black box — shown by a boot staged with a `/system` that does
not serve.
