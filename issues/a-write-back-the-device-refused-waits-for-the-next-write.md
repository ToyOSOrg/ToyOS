---
status: open
kind: defect
opened: 2026-09-27
---

# A write-back the device refused waits for the next write

fsd's write-back (`userland/fsd/src/writeback.rs`) is spent when it comes due,
and only a sync that answers is recorded: one that left a file unwritten stays
due and is tried again. A sync the volume refuses whole — the cache's flush or
the device — returns before that, so the write-back is spent and nothing
syncs the dirty blocks the cache still holds until a client writes or syncs
again.

Owner: the fsd server loop (`userland/fsd/src/main.rs`).

**Exit**: a refused sync keeps the write-back due at a bound that does not
say the refusal once every two seconds for the rest of the boot, with a host
test of `WriteBack` over a refused sync.
