---
status: open
kind: defect
opened: 2026-09-30
---

# An IPI can overtake the stores it announces

Neither architecture orders a CPU's earlier stores before the interrupt it
raises to announce them, so on hardware the target can take the interrupt and
read the word it was sent for before that word has landed.

- **AArch64**: `kernel/src/arch/aarch64/irqchip.rs`'s `raise` writes
  `ICC_SGI1R_EL1` with no barrier before it. Only a `DSB` orders a system
  register write after stores to Normal memory; Linux's `gic_ipi_send_mask`
  (`drivers/irqchip/irq-gic-v3.c`) issues `dsb(ishst)` before the write for
  exactly this.
- **x86-64**: `kernel/src/arch/x86_64/apic.rs`'s `Reg::write` of the ICR is a
  `wrmsr`, which in x2APIC mode is not serializing (SDM Vol. 3A, "MSR Access
  in x2APIC Mode"); Linux fences every x2APIC IPI with `weak_wrmsr_fence`
  (`mfence; lfence`).

One reader that depends on the order: `kernel/src/sched/dump.rs` stores
`OWES` for each sibling, then kicks it, and a sibling that takes its kick
before its `OWES` is visible answers nothing, which the dump then reports as
a silent CPU.

No guest here reds on it, and none is expected to: under TCG the write is a
call into QEMU's interrupt controller model and under HVF a trap, and neither
is the hardware's ordering. The evidence is the architecture and Linux's code,
and the exit is the barrier on both, one instruction each.
