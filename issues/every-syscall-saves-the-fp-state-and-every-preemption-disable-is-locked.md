---
status: assigned
kind: defect
opened: 2026-10-12
---

# Every syscall saves the FP state, and every preemption disable is a locked instruction

The syscall stub parks the user's whole FP/SSE image on the kernel stack with
`fxsave64` and restores it with `fxrstor64` on the way out
(`kernel/src/arch/x86_64/entry.rs:98-121`, `syscall.rs:61`), whether or not the
thread is switched out, though the kernel is built soft-float and never touches
those registers (`build/build.rs:511-522`). Saving at the ring transition is
what lets a switch skip it; every syscall that returns to its own thread pays
it anyway. PR #859, open, makes the image an XSAVE area: under XSAVES the T14
would park 2432 bytes, computed and not yet read from a boot (that PR's body).

Every preemption disable on x86_64, `percpu::preempt_count_up` and
`preempt_count_down` (`kernel/src/arch/x86_64/percpu.rs:791-802`), is a
`lock add` or `lock sub` on the CPU's own count (`gs::lock_inc_u32` and
`lock_dec_u32`, `percpu.rs:299-315`). The word is private to the CPU, and the
reason given for the prefix, that an interrupt can split a plain `add`'s load
and store, is not the hardware's: an interrupt is taken only on an
instruction boundary (SDM Vol. 3A §6.6). The syscall stub's own raise and
lower around the handler (`syscall.rs:63`, `:67`) are the same instructions,
but they are `issues/syscall-preemption-is-incidental.md`'s: that file's step
of `issues/toyos-beats-linuxs-latency-on-the-t14.md` removes the raise, and
this file does not.

A null syscall (`SYS_GETPID`, `syscall_cost`) reads 650 TSC cycles on the T14
at `c126cd9d5` (2419 MHz,
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6114829103) and 384 on
the AMD bench at `fb03703a1` (1796 MHz,
https://github.com/ToyOSOrg/ToyOS/pull/852#issuecomment-6107031259). How
much of either is the save and the locked instructions is unmeasured.

`wt/toyos-scout-sys` holds the split: each part of the entry and exit timed on
both benches.

**Exit**: that split, posted; the save gone from a syscall that returns to its
own thread, and the `lock` prefix gone from `preempt_count_up` and
`preempt_count_down`, or either kept with its refusal reason at its site and
its measured cost on both benches in the closing commit; and `syscall_cost`
read on both benches before and after.
