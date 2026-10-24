---
status: open
kind: defect
opened: 2026-10-01
---

# A fatal path inside its own CPU's console burst waits out the bound

A fatal path takes the console registers by `BackendLock::try_seize`
(`kernel/src/drivers/serial_lock.rs`), which finds a hold its own CPU's fatal
path left, and `serial::panic_registers` gives up on any other after
`PANIC_LOCK_WAIT`. A burst holder — `write_wire`'s burst, a byte read —
holds them as `LIVE`, which names no CPU. A fault, a panic or a nested NMI that lands inside one waits the whole
bound for a holder that never runs again, says so, and writes over it; every
later `panic_registers` on that path waits it again.

**Evidence:** the code.

**Exit:** no fatal path waits out a hold its own CPU left.
