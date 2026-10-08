---
status: open
kind: finding
opened: 2026-10-08
---

# A thread's entry is never checked, and what a noncanonical one does is measured only under TCG

`sys_thread_spawn` (`kernel/src/syscall/proc.rs`) checks `stack_base <=
stack_ptr` and nothing else. The entry reaches `thread_start`
(`kernel/src/arch/x86_64/entry.rs`) unchanged and is the `RIP` its `iretq`
returns to, so what an entry that is no canonical address does is decided by
the CPU and not by the kernel.

**Measured under QEMU TCG** (an x86-64 guest on an AArch64 host, `tests/testcases`,
two CPUs, at `b432ed21c`): a guest binary that spawns a thread at such an entry
over a mapped stack. The spawn was accepted, the fault was taken with a Ring 3
frame, and the kernel ended that process alone:

```
FAULT rip=0x0100000000000000 cr2=0x0000000000000000 err=0x0000000000000000 ... tid=1
SIGBUS tid=1: general protection fault (error_code=0x0)
    cs=0x0023  ss=0x001b  rflags=0x0000000000010202
exit: test_rs_spawn_noncanonical_ pid=10 code=-1 cpu=9ms
===TEST_END test_rs_spawn_noncanonical_entry exit=-1===
```

The machine went on and the harness finished its run. So the emulator completes
the return and faults at the fetch.

**Not measured on hardware.** Intel's SDM (Volume 2, `IRET`) raises `#GP(0)` for
a return `RIP` that is not canonical, from the instruction itself — a Ring 0
frame, which `fatal_exception`
(`kernel/src/arch/x86_64/idt/exceptions.rs`) answers with `halt_all_cpus()`,
since only a Ring 3 fault ends a process. If the T14, an Intel machine, does
what that document says, one process halts it. Nothing has executed that; TCG
cannot, and the T14 is the orchestrator's.

The binary and its registration are a patch on the pull request that filed
this.

**Exit**: a metal row runs that binary on the T14. If the machine halts this is
a `defect` whose exit is that `SYS_THREAD_SPAWN` refuses an entry outside the
canonical user half, by name, on every machine; if the process ends alone
there too, the one line this leaves is `sys_thread_spawn`'s doc comment saying
the entry is the CPU's to refuse.
