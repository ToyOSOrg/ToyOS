---
status: assigned
kind: defect
opened: 2026-10-12
---

# Every syscall saves the FP state and takes two locked instructions

The syscall stub parks the user's whole FP/SSE image on the kernel stack with
`fxsave64` and restores it with `fxrstor64` on the way out
(`kernel/src/arch/x86_64/entry.rs:98-121`, `syscall.rs:61`), whether or not the
thread is switched out, though the kernel is built soft-float and never touches
those registers (`build/build.rs:511-522`). Saving at the ring transition is
what lets a switch skip it; every syscall that returns to its own thread pays
it anyway. PR #859, open, makes the image an XSAVE area, 2688 bytes on the T14.

Around the handler the stub raises and lowers the CPU's own preemption count
with `lock add` and `lock sub` (`syscall.rs:63`, `:67`; the same primitive is
`kernel/src/arch/x86_64/percpu.rs:299-315`). The word is private to the CPU,
and the reason given for the prefix, that an interrupt can split a plain
`add`'s load and store, is not the hardware's: an interrupt is taken only on an
instruction boundary (SDM Vol. 3A §6.6).

A null syscall (`SYS_GETPID`, `syscall_cost`) reads 650 TSC cycles on the T14
at `c126cd9d5` (2419 MHz,
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6114829103) and 384 on
the AMD bench at `fb03703a1` (1796 MHz,
https://github.com/ToyOSOrg/ToyOS/pull/852#issuecomment-6107031259). How
much of either is the save and the two locked instructions is unmeasured.

`wt/toyos-scout-sys` holds the split: each part of the entry and exit timed on
both benches.

**Exit**: that split, posted; the save and each locked instruction either gone
from a syscall that returns to its own thread, or kept with its measured cost
stated at the stub; and `syscall_cost` read on both benches before and after.
