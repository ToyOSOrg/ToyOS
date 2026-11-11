---
status: open
kind: finding
opened: 2026-11-11
---

# The kernel stacks' range keeps every page table it ever made

A thread's kernel stack is mapped in a slot of a kernel virtual range of its
own (`kernel/src/mm/stack.rs`). The stack's frames are its job's and go back
when it drops. The range's page tables are the kernel's, charged to no job,
and are never freed: a dropped stack's slot is reused only after a flush, and
freeing a table without one would leave it named by another CPU's
paging-structure cache.

The bound is one 4 KiB table for each 8 slots ever in use at once, at most 64
slots more than the most threads alive at once, and one directory for each
4096 slots: 2 MiB of tables for a process at its 4096-thread limit. A spawn
that cannot get a frame for one is refused.

Not measured: no boot has read how many tables the range holds.

**Exit**: the owner accepted this bound when the range was designed; this
record goes when a census line shows the tables' count on a machine, or a
workload shows the bound mattering.
