---
status: open
kind: defect
opened: 2026-10-08
---

# A noncanonical thread entry halts the kernel at its Ring 0 return

`sys_thread_spawn` (`kernel/src/syscall/proc.rs:132`) validates only
`stack_base <= stack_ptr`. The entry argument is never checked for the user
half or for canonical form. It is carried unchanged through
`process::spawn_thread` and `alloc_kernel_stack` into `r12` of `thread_start`
(`kernel/src/arch/x86_64/entry.rs`), which pushes it as the return `RIP` and
executes `iretq`.

A noncanonical entry (noncanonical under 48-bit and 57-bit paging alike) makes
the `iretq` fault *before* the privilege transition completes, so the `#GP`
frame names the kernel code segment. `fatal_exception`
(`kernel/src/arch/x86_64/idt/exceptions.rs`, the `ring().is_user()` branch near
the end) ends only Ring 3 faults as the current process and calls
`halt_all_cpus()` for a Ring 0 fault. An unmapped but canonical entry faults in
Ring 3 and is survivable; the noncanonical case is not. One ordinary process
can therefore halt the machine — untrusted input reaching the kernel as a
crash, which the kernel must refuse.

A kernel-half canonical entry (e.g. the direct-map region) is the same class:
the entry is used as a user `RIP` with no bound.

**Exit**: a process that calls `SYS_THREAD_SPAWN` with a noncanonical entry, and
one with a canonical kernel-half entry, over a valid mapped stack, is refused at
the syscall or ends alone; a separate witness process keeps running and the
kernel neither halts nor resets. A guest test that spawns such a thread reds
today (the machine halts) and passes once the entry is bounds-checked.
