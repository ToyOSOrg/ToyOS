---
status: open
kind: defect
opened: 2026-10-08
---

# A region keeps the address space of every process that ended with it mapped

`SharedMemObject::map_into` (`kernel/src/object/shm.rs`) records each mapping
as `(Pid, PageTables, UserAddr)`, and `PageTables` is the `Arc` of the
process's address space. An entry leaves the list in two places only: the
region's zero-handle hook, when the last handle to it goes anywhere, and
`unmap_from`, which nothing but an inbox's teardown calls
(`kernel/src/inbox/mod.rs`). A process's own teardown
(`teardown_resources`, `kernel/src/process.rs`) closes its handles and
touches no region's list.

So a process that maps a region somebody else also holds, and ends, leaves
its entry behind, and the entry keeps that process's `AddressSpace` alive
until the region's last holder lets go: its page-table pages, whatever its
`pages` map still owns, and its user PCID, which returns to the pool only
when the space drops (`PcidGuard`, `kernel/src/arch/x86_64/paging.rs`). The
list grows by one entry per such process and nothing bounds it. A region
handle carries `DUP` and `TRANSFER`, so one process can keep a region and
have child after child map it and end; the pool holds
`kernel::pcid::MAX_USER_PCID` tags, and with every one held
`AddressSpace::new_user` answers `None` and every spawn on the machine is
refused `ResourceExhausted` (`kernel/src/loader/mod.rs`).

**Read from the code, not run.** Pids are never reissued
(`kernel/pure/proclife/pids.rs`), so a later process is never answered a dead
one's address by the list's `find`.

**Exit condition**: a process's teardown leaves no entry of it in any
region's list — and a test in which a child maps a region its parent keeps
and ends sees the child's address space freed, its PCID back in the pool,
while the parent still holds the handle.

**Owner**: `kernel::object::shm`'s handle-driven mapping teardown, with
`issues/the-compositor-keeps-a-committed-regions-mapping-for-as-long-as-the-client-does.md`,
whose exit ends a mapping at its process's last handle and so reaches this
one only where the process still held a handle when it ended.
