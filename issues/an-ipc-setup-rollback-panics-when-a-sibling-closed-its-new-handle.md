---
status: open
kind: defect
opened: 2026-10-08
---

# An IPC setup rollback panics when a sibling closed its new handle first

`sys_inbox_setup` (`kernel/src/syscall/ipc.rs:444`) installs the inbox handle
under the process-data lock, releases that lock, and then runs `ctx.copy_out`
to a user address. When the copy fails the rollback closes the handle with
`ops::close(...).expect("the inbox this call installed a moment ago")`
(`:466`). `sys_namespace_open` (`:219`) has the same shape: it installs the
client handle, releases the lock, pushes onto the port's pending queue, and on
`QueueFull`/`Closed` rolls back with
`ops::close(...).expect("the connection this call installed a moment ago")`
(`:265`).

The install is visible to sibling threads the instant the process-data lock is
dropped, and the fallible step (`copy_out`, or the queue push) runs with no
reservation held. A sibling thread of the same process, on another CPU, can
`SYS_CLOSE` that handle — or move it with `SYS_HANDLE_SEND` — between install
and rollback. The rollback's `HandleTable::remove`
(`kernel/src/object/handle.rs`) then returns `Stale`/an error, and the `expect`
turns a correctly detected concurrent close into a kernel panic. The comments'
premise — "the handle this call installed a moment ago" is still held — is not
an invariant across the dropped lock. Untrusted ordering panics the kernel
where it must refuse.

**Exit**: with a sibling closing (or sending) the just-installed handle while
`SYS_INBOX_SETUP` is in its failing-`copy_out` rollback, and while
`SYS_NAMESPACE_OPEN` is in its queue-full rollback, both syscalls return their
original refusal and the kernel does not panic. **Traced, not executed.** No model compiles the handle table, and staging the
window needs an actuator the kernel does not have: one that holds the
installing thread between its install and its fallible step until a sibling
releases it.
