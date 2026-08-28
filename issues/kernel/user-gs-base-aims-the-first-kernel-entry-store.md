---
status: open
kind: defect
opened: 2026-08-28
---

# `CR4.FSGSBASE` is required and nothing swaps `GS`, so a Ring 3 `wrgsbase` aims the first store of every kernel entry

Every kernel entry path's first memory access is `gs:`-relative, and the base it goes through is a register Ring 3 can write. An unprivileged process turns one instruction into an arbitrary kernel write.

## The mechanism

`cr4::FSGSBASE` is in `CR4_REQUIRED` (`kernel/src/arch/control_regs.rs:57-62`), and `declaration` panics the boot on a CPU that lacks it (`kernel/src/arch/control_regs.rs:140-145`), so `CR4.FSGSBASE=1` on every CPU for the machine's life — the only other `write_cr4` callers, `kernel/src/mm/../arch/pat.rs:84-85`, clear and restore PGE from the live value and never touch bit 16. `tests/toyos.rs:12066-12072` asserts the bit set on every CPU, so this is a checked invariant rather than an accident.

The comment at `kernel/src/arch/control_regs.rs:54-56` gives the reason as "context switch uses `rdfsbase`/`wrfsbase` unconditionally" (`kernel/src/hw.rs:407,429`). But `CR4.FSGSBASE` is one bit and it enables all four instructions at any CPL: taking `WRFSBASE` for the kernel hands `WRGSBASE` to Ring 3, and no CR4 bit separates them.

`IA32_GS_BASE` is written once per CPU and never again — `kernel/src/arch/percpu.rs:586` on the BSP, `kernel/src/arch/smp.rs:366-373` in the AP trampoline. There is no `swapgs` in this kernel and no use of `IA32_KERNEL_GS_BASE` (MSR `0xC000_0102`), the split-MSR mechanism that exists to make `WRGSBASE` harmless; `kernel/src/arch/syscall/gate.rs:40` states the design as it stands: "GS permanently points to kernel per-CPU data here; no swapgs", and `kernel/src/arch/percpu.rs:160` keeps the selector unreloaded so the MSR-loaded base survives. Nothing per-thread saves or restores it either: `KernelHw::switch` carries `fs_base` and has no GS counterpart (`kernel/src/hw.rs:405-429`, `kernel/src/sched/driver.rs:400`), so a base a thread writes stays live on that CPU across context switches.

`kernel/src/arch/percpu.rs:204-206` writes the assumption down and marks it unchecked: "Caller-owed and unchecked: `GS_BASE` must already point at this CPU's `PerCpu`." Nothing establishes it after boot.

## The chain on current main

`ring3_naked_asm!` prepends exactly one instruction, `cld` (`kernel/src/arch/entry.rs:50-63`) — no GS reload, no check. So the first memory-touching instruction of each entry addresses through the user's register:

- `kernel/src/arch/syscall/gate.rs:45` — `mov gs:[{user_rsp}], rsp`, before the stack switch at `:46` and before any validation. Address from `GS.base`, value the caller's own `rsp`.
- `kernel/src/arch/syscall/gate.rs:46` — `mov rsp, gs:[{kernel_rsp}]` loads the kernel stack pointer from that same base.
- `kernel/src/arch/idt/mod.rs:315` — `lock add dword ptr gs:[{preempt_count}], 1` in `common_entry`, every exception vector's second half.
- `kernel/src/arch/idt/device_irq.rs:27` — the same, in the macro every device stub expands.
- `kernel/src/arch/idt/timer.rs:25,45,46,55,58,60` — including `gs:[{armed_ticks}]` read as the value written into the x2APIC timer MSR.
- `kernel/src/arch/idt/nmi.rs:30` — `cmp dword ptr gs:[{active}], 0`, the first instruction after `cld`.

`kernel/src/mm/paging.rs:360` records that `PML4[256..511]` is the shared kernel half, so a kernel virtual address the attacker picks is mapped while their own `CR3` is live, and SMAP — which only bars supervisor access to *user-accessible* pages — does not constrain a kernel-address target at all.

## Impact

A complete unprivileged Ring 3 → Ring 0 isolation break, not merely a crash. `PerCpu.kernel_rsp` and `PerCpu.user_rsp` are consecutive `u64`s under `#[repr(C)]` (`kernel/src/arch/percpu.rs:70-72`), and `gate.rs:45-46` writes the second then loads `rsp` from the first: a first `GS.base` whose `user_rsp` slot overlaps a second `GS.base`'s `kernel_rsp` slot lets one syscall plant a value the next syscall loads directly into `rsp`, pivoting the kernel stack to attacker-chosen memory before the entry's pushes and its `sysretq`. Because nothing restores GS base, the corruption persists on that CPU.

The degraded arm is a doctrine violation on its own: a `GS.base` aimed at a user-accessible page makes `gate.rs:45` a supervisor write to a user page with `RFLAGS.AC` clear (`gate.rs` masks `AC` in `IA32_FMASK` precisely so SMAP binds), taking `#PF` while still at CPL 0 on the user stack — the window `kernel/src/arch/idt/nmi.rs:7-8` names — whose frame push faults again into `#DF`. Unprivileged userland halts the machine on demand, against "the kernel never crashes from userland".

Userland here includes network-facing servers (`/bin/sshd`, netd) and third-party code (doomgeneric, toybox), so any userland code-execution bug upgrades straight to kernel compromise instead of stopping at the process boundary.

## Precondition and repro

None beyond running a Ring 3 thread. From any process: execute `wrgsbase rax` with a chosen canonical value, then either issue any syscall or simply wait — the next timer tick (`kernel/src/arch/idt/timer.rs`) or any device IRQ (`kernel/src/arch/idt/device_irq.rs:27`) touches `gs:` with no action from the attacker at all. No race, no capability, no handle.

`kernel/src/arch/syscall/vm.rs:352-355` already refuses to trust the FS base for exactly this reason — "never by chasing a pointer out of the FS base: `FSGSBASE` lets userland set that register" — so the mechanism is known; the hygiene was simply never carried across to GS.

## Fix direction

Two answers, and they are not equivalent in cost:

- **Take the bit away.** Drop `cr4::FSGSBASE` from `CR4_REQUIRED` and give the kernel its FS base through `IA32_FS_BASE` (`0xC000_0100`) `rdmsr`/`wrmsr` instead of `rdfsbase`/`wrfsbase`. `kernel/src/arch/cpu.rs:256-269` are the only two wrappers and `kernel/src/hw.rs:407,429` plus `kernel/src/process.rs:1548` the only callers, so the change is small and it removes the primitive rather than working around it. It costs an MSR write per context switch against a `wrfsbase`, and it must be priced against the switch instruments before it is taken.
- **Use the mechanism the ISA provides.** Park the per-CPU pointer in `IA32_KERNEL_GS_BASE` and `swapgs` on every Ring 3 entry and exit. This is the larger change: every entry in the chain above needs the swap, and `nmi_entry` and `common_entry` need the CPL-aware form (an NMI or exception can land in Ring 0 with the kernel base already loaded, so an unconditional `swapgs` there is itself the bug), which means the entry paths grow a `paranoid`-style discrimination this kernel does not have today.

Whichever is taken, the two checks it must name: the negative control is the current unswapped entry restored whole under a feature gate, with the exploit test going from refused to a witnessed write; the independent oracle is the SDM's own statement that `CR4.FSGSBASE` gates all four instructions at any CPL (Vol. 3A §2.5, Vol. 2 `WRGSBASE`) — and the gate this needs is a userland test program that executes `wrgsbase` with a kernel address and then issues a syscall, where the machine must survive with its per-CPU state intact.
