---
status: open
kind: track
opened: 2026-09-29
---

# A TLB shootdown reaches only the CPUs that ran the space

A shootdown blocks on every other CPU (`kernel/src/mm/unmapped.rs:5`),
whether or not it ever ran the address space. It waits on
`issues/kernel/no-test-can-hold-a-thread-on-a-named-cpu.md`. A CPU that
switches away keeps the space's entries: under PCID every CR3 load sets
NOFLUSH (`kernel/src/arch/x86_64/paging.rs:295-306`), so a CPU leaves the
target set only when those entries are gone, not when it stops running the
space. The TCG model has no PCID (`src/arch.rs:169`), so only a machine with
PCID can show a switched-out CPU still holding them.

**Exit**: on every proving machine and under TCG, a sibling pinned on another
CPU touches a page the initiator unmaps and then faults; on each proving
machine with PCID, the sibling touches the page and blocks off its pinned CPU,
the initiator unmaps it while the sibling is blocked and a thread of another
space runs there, and the sibling faults on touching it again after it wakes
there; cases in `kernel/loom/tests/tlb_shootdown.rs` switch a CPU into the
space, and out of it, while the initiator reads the target set; the T14's
munmap figure. **Mutation**, each red: a target set that omits a CPU the space
ran on; a CPU that joins the set after loading CR3; a CPU cleared from the set
when it switches away from the space, its red recorded over repeated runs,
since it rests on the TLB keeping the entry across the block. **Oracle**: loom,
and each proving machine's TLB.
