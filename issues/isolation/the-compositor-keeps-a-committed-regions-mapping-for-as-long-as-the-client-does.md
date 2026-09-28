---
status: open
kind: defect
opened: 2026-09-28
---

# The compositor keeps a committed region's mapping for as long as the client does

`CopyRegion::take` (`userland/compositor/src/client.rs`) drops the
compositor's own `SharedMemory`, closing its handle to the region. That does
not unmap it: `SharedMemObject::on_zero_handles` (`kernel/src/object/shm.rs`)
tears down every process's mapping together, only once every handle to the
object is gone anywhere — `unmap_from` exists to drop one process's own
mapping on its own, but nothing outside `kernel/src/inbox/mod.rs` calls it. A
client that keeps the handle its copy answered with keeps the compositor's own
2 MiB mapping alive too, for as long as it likes, and a client that copies
repeatedly and keeps every handle leaves one such mapping per copy — bounded
only by its own handle table, never by the compositor's need for the memory.

Owner: `kernel::object::shm`'s handle-driven mapping teardown.

**Exit**: closing a process's last handle to a shared-memory object unmaps
that process's own view at once, through `unmap_from`, independent of whether
another process still holds a handle to the same object.
