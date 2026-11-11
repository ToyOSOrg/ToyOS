---
status: open
kind: finding
opened: 2026-11-11
---

# A namespace is a kernel object, and service lookup is kept out of the kernel

An open design question, the orchestrator's to resolve. Found by the
inventory in
`issues/the-kernel-is-small-interrupts-post-and-threads-wait.md`.

The kernel holds a table of service names to connectors per namespace
(`kernel/src/object/namespace.rs`) and three calls over it,
`SYS_NAMESPACE_BUILD`, `SYS_NAMESPACE_OPEN` and `SYS_NAMESPACE_NAMES`
(`kernel/src/syscall/ipc.rs`), about 215 lines: it copies name text of up
to 64 KiB (`SYS_NAMESPACE_OPEN`, through `user_str`), compares it and lists
it. The design keeps a namespace as an immutable kernel object and, in its
statement of what stays out of the kernel, names service lookup. Both
cannot be the whole truth of this code.

The two readings: the object is the mechanism and "lookup" means a global
registry, which the kernel has none of, so nothing moves; or name text is
userland's, a process is endowed connectors and its parent's library maps
names to them, and the kernel's table and three calls go. The second is an
inference, not a design anyone wrote.

**Exit**: one reading is chosen and the design says it in one place; under
the first this file is deleted, under the second it becomes a stage of the
track.
