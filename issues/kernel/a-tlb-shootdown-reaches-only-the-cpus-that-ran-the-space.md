---
status: open
kind: track
opened: 2026-09-29
---

# A TLB shootdown reaches only the CPUs that ran the space

A shootdown blocks on every other CPU (`kernel/src/mm/unmapped.rs:5`),
whether or not it ever ran the address space. It waits on
`issues/kernel/no-test-can-hold-a-thread-on-a-named-cpu.md`.

**Exit**: on every proving machine and under TCG, a sibling pinned on another
CPU touches a page the initiator unmaps and then faults; a case in
`kernel-loom/tests/tlb_shootdown.rs` switches a CPU into the space while the
initiator reads the target set; the T14's munmap figure. **Mutation**: a
target set that omits a CPU the space ran on; a CPU that joins the set after
loading CR3. **Oracle**: loom.
