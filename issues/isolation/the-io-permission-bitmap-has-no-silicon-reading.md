---
status: open
kind: tooling
opened: 2026-09-29
---

# The I/O permission bitmap has no silicon reading

What an `isa` claim opens, and every port it leaves closed, rests on the TSS I/O
permission bitmap and its limit (`kernel/src/arch/x86_64/pio.rs`,
`kernel/src/arch/x86_64/percpu.rs`). The guest tests that read it
(`isa_ports_are_the_binders_alone`, `isa_ports_close_on_one_cpu`) run locally
under TCG, whose `check_io` is QEMU's own implementation of the check; the
processor's reading comes only from KVM, which only `nightly.yml` runs, and from
metal, which has not run them.

**Exit**: both tests green on a KVM run of the branch that carries the bitmap,
or on the T14.
