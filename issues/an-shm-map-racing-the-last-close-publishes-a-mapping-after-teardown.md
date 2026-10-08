---
status: open
kind: defect
opened: 2026-10-08
---

# A shared-memory map racing the last close publishes a mapping after teardown

`sys_shm_map` (`kernel/src/syscall/ipc.rs:361`) resolves the handle and clones
the object's `Arc` under the process-data lock, releases the lock, and then
calls `SharedMemObject::map_into` (`kernel/src/object/shm.rs:132`). `map_into`
never checks whether the object has retired; it allocates a writable mapping and
appends it to `mapped_in`.

A cloned `Arc` held by a syscall is not a handle and does not postpone
zero-handle retirement. If a sibling closes the last handle after the clone and
before `map_into`, the object retires and `on_zero_handles`
(`kernel/src/object/shm.rs:172`) runs once: it takes `mapped_in`, finds it empty
and returns, and its queued `Arc` drops. When `map_into` then appends the new
mapping, nothing will ever tear it down — `on_zero_handles` has already run and
will not run again.

With debug assertions on (the guest/test profile sets `debug-assertions = true`
in `kernel/Cargo.toml`), the final `Arc` drop trips the `debug_assert!` in
`SharedMemObject::drop` (`:186`) and panics the kernel. Without them, the
region's owned pages return to the PMM while the page-table entries stay
present, so the surviving process can read or write pages after another process
or the kernel receives them. This is distinct from the release-latency recorded
in `issues/deferred-release-outlives-its-syscall.md`: there memory is reclaimed
safely but late; here a mapping is created *after* the one teardown, so the
pages are freed while still mapped.

**Exit**: a `SYS_SHM_MAP` that races the last close of its handle either maps
nothing or is torn down with the object; no shm object's pages are present in
any page table after its zero-handle teardown has run, and the kernel does not
panic. **Traced, not executed.** No model compiles the object layer, and staging the
window needs an actuator the kernel does not have: one that holds `sys_shm_map`
between its clone and `map_into` until a sibling releases it.
