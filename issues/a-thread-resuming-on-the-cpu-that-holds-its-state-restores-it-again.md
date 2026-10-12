---
status: open
kind: defect
opened: 2026-10-12
---

# A thread resuming on the CPU that holds its state restores it again

`KernelHw::switch` (`kernel/src/arch/x86_64/hw.rs`) runs `fpu::restore`, an
`XRSTORS` or `XRSTOR` with the full mask, every time a thread of a process
resumes. When the thread comes back to the CPU it last ran on and nothing
loaded another thread's state there in between, as in A → idle → A or A → a
kernel thread → A, the registers already hold exactly what that restore
loads.

Linux skips that restore. Its test is `fpregs_state_valid`: the CPU's
`fpu_fpregs_owner_ctx` still names the task's FPU, and that FPU's `last_cpu`
is this CPU. The review of PR #871
(https://github.com/ToyOSOrg/ToyOS/pull/871#issuecomment-6125938770) cites it
in v4.19's `switch_fpu_finish` (`arch/x86/include/asm/fpu/internal.h`) and in
v6.10's `fpregs_restore_userregs` (`arch/x86/kernel/fpu/context.h`), so both
its eager and its deferred eras skip it. The design this follows is Linux's:
a per-CPU owner and a per-thread last CPU, the restore skipped when each names
the other, and the owner cleared by every load of other state onto the CPU,
which is another thread's restore or a new thread's `fpu::load_initial`.

The skip keeps `arch::fpu`'s invariant: a skipped restore leaves the CPU's
last restore from the thread's own area, and nothing but that thread's saves
has written the area since.

Owner: `issues/toyos-uses-what-modern-hardware-offers-for-speed.md`.

**Exit**: a thread that resumes on the CPU whose registers still hold its
state is not restored, by Linux's test. A mutant whose owner is never cleared
turns `vector_state` red on the T14. The cost of a switch with and without the
skip is reported from both benches, and no bound gates it.
