---
status: open
kind: defect
opened: 2026-10-01
---

# A panic reentry inside a fatal path's console hold halts with it held

`panic::last_words` holds the console registers (`serial::panic_registers`)
for its report, and `serial::panic_flush` for its drain. On the DOUBLE PANIC
path and the first `panic_flush` of an ordinary panic (`kernel/src/main.rs`),
that is before `halt_all_cpus` has stopped the other CPUs. A panic inside the
hold reaches the reentry guard, which writes under it and halts this CPU
alone, so the hold is never let go: every other CPU's next console burst
waits for it with interrupts off (`BackendLock::lock`), and the console takes
no burst again.

**Evidence:** the code.

**Exit:** no CPU halts holding the console registers while another still runs.
