---
status: open
kind: defect
opened: 2026-09-29
---

# Every syscall runs at one kernel stack offset

`syscall_entry` loads the thread's fixed stack top
(`kernel/src/arch/x86_64/syscall.rs:107`), so each of a thread's syscall
frames sits at the same address on every syscall, and a stack address leaked
by one syscall or an overwrite laid out against one frame carries to the next.
Linux at `Ubuntu-6.8.0-142.142`, under the T14's
`CONFIG_RANDOMIZE_KSTACK_OFFSET_DEFAULT=y`, moves the stack by a fresh
offset at every syscall entry (`arch/x86/entry/common.c:73`,
`include/linux/randomize_kstack.h:40,50`), drawn on the exit before it
(`arch/x86/include/asm/entry-common.h:85`), 7 bits after alignment on x86-64
(`entry-common.h:76-83`). A row of the hardening table in
`issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`.

**Exit**: every syscall's handler runs below an offset drawn per syscall, of at
least 7 bits; a `boot-actuators` probe records one handler frame's address
over 1000 syscalls and finds each claimed bit both set and clear, and a
constant offset reds it.
