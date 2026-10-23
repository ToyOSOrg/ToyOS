---
status: open
kind: defect
opened: 2026-10-22
---

# Two CPUs faulting at once write one black box

Every exception entry but a Ring 3 page fault seals its registers into the
black box before its handler runs (`kernel/src/arch/x86_64/idt/mod.rs`,
`trap_dispatch`, into `kernel/src/blackbox.rs`'s `record_fault`). The box is
one page set for the machine, and `with_page` hands each writer a `&mut` over
it with no exclusion between CPUs: two CPUs taking a kernel-mode fault or an
exception at the same moment write it at once. That is an aliasing
`&mut` and a data race, and one record can tear the other's under a checksum
the second writer computed, so the next boot reads a record no CPU wrote.

Rare: a kernel-mode fault is a kernel bug, and the Ring 3 page fault, the one
ordinary work takes, no longer seals. Found in review of #1008, which stopped
user page faults from sealing.

Owner: the black box. Exit: an exception record per CPU, or one exclusion
that cannot deadlock an exception entry, with a host test of two writers.
