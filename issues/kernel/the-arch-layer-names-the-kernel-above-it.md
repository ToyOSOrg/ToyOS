---
status: open
kind: defect
opened: 2026-09-27
---

# The arch layer names the kernel above it

`kernel/src/arch/` is meant to be the machine and nothing else: generic code
reaches it through `crate::arch::…`, and it reaches nothing back. It does. Its
files name kernel modules above it by path — the scheduler, the process table,
the log, drivers, the IOMMU layer, test actuators — and `ARCH_REACHES_UP` in
`src/sourcegate.rs` enumerates every such name, per file. The gate beside it
reds on a name a file adds and on a listed name a file no longer spells, so the
list only shrinks; each entry is this file's debt.

What the list holds, by kind:

- **Upcalls a contract would name.** Trap and syscall entry into the
  scheduler and `syscall`, the log, `mm`, `clock`/`time`. These stay, but
  through an interface generic code declares, not a path the architecture
  picks.
- **PC platform devices living in the ISA layer.** The i8042 driving
  `keyboard`, `mouse` and `irq_ring`; VT-d naming `iommu`, `pcidev` and
  `drivers::{acpi, pci}`; the per-device vectors under `idt/` naming `drivers` and
  `pcidev`; the TCO watchdog reading `params`.
- **Kernel state the architecture stores.** Per-CPU data holding
  `process::{Pid, Tid}`.
- **Test hooks.** `actuator`, across boot, SMP, the i8042, VT-d and the
  interrupt entry.

Why it matters: the AArch64 port inherits whatever x86-64 exports, so every
upward name here is one the port has to satisfy or stub, and until the gate
nothing but review stopped the next.

Owed by the arch-contract work: an `arch/api.rs` contract, a trait implemented
by a zero-sized type and dispatched statically, that is the only way generic
code reaches the machine and the only way the machine reaches back; and
`platform/{pc,virt}` for the PC's own devices, so `arch/` is the ISA alone.

**Exit**: `ARCH_REACHES_UP` is empty.
