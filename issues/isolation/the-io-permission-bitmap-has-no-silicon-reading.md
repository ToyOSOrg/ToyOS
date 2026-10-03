---
status: open
kind: tooling
opened: 2026-09-29
---

# The I/O permission bitmap has no silicon reading

What an `isa` claim opens, and every port it leaves closed, rests on the TSS I/O
permission bitmap and its limit (`toyos-userbound/src/port.rs`,
`kernel/src/arch/x86_64/percpu.rs`). The row that reads it,
`isa_ports_are_the_binders_alone`, is the T14's, and the T14 has not run it. Its
mutations were measured in a QEMU guest under TCG, whose `check_io` is QEMU's
own implementation of the processor's check.

**Exit**: the row green on the T14.
