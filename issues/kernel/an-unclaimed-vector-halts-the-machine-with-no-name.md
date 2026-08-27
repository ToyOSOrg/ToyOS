---
status: open
kind: defect
opened: 2026-08-27
---

# 235 IDT entries are `P = 0`, and a delivery through one halts the machine saying nothing

`kernel/src/arch/idt/mod.rs` fills the table from `idt_vectors!` and leaves
every other slot `IdtEntry::EMPTY`, whose `type_attr` is `0`. A vector delivered
through a gate with `P = 0` is not a fault the process takes: the CPU treats the
missing gate as a second, contributory fault and escalates to `#DF`, which
`double_fault_handler` answers with `halt_all_cpus`.

## Measured, on this tree

Taken as the negative control of the spurious-vector gate (2026-08-27): with
that gate's row removed and the delivery still staged, the guest never reaches
`===READY===` and the harness reports **`the console carried: nothing at all`**.
Not a report, not a `DOUBLE FAULT` line, not a panic — a machine that stopped
with no name on it, which is the whole of what this entry is about. With the row
in place the same boot is green.

## What would close it

A single naked entry installed in every otherwise-unfilled slot, which counts
the vector it took and reports it — the shape `arch/idt/spurious.rs` already
has, minus the conditional EOI question, which it inherits: an unexpected
vector may or may not be in service and the handler has to ask before it
acknowledges.

The reachable ones are not hypothetical. A stale MSI-X table entry left by a
driver reconfiguration names a vector nothing gated; so does firmware that left
an I/O APIC redirection entry pointing somewhere this kernel does not, which
`ioapic::init` masks precisely because of it.

**What it costs to get wrong**: a gate that absorbs a vector silently is a
machine hiding an interrupt-routing defect, so the count and the report are the
point, not the survival.
