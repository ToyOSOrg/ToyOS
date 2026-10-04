---
status: open
kind: defect
opened: 2026-10-04
---

# A fresh kernel heap chunk holds its frame's last owner's bytes

When the kernel heap grows, `KernelAllocator::alloc` (`kernel/src/mm/alloc.rs`)
takes its frame from `pmm::claim` (`kernel/src/mm/pmm.rs`), which does not zero
it, so that no 2 MiB write runs while a CPU waits on the heap lock. A chunk
carved from that frame holds whatever its last owner, a process's page among
them, left there until the kernel writes it. `GlobalAlloc::alloc` promises no
contents either way, but a kernel bug that copies out heap memory it never
wrote now leaks another process's data where it used to leak zeros.

By reading, not measured: no reachable copy-out of unwritten heap memory is
known.

Owner: `issues/the-kernel-is-at-least-as-secure-as-linux-on-every-machine-toyos-supports.md`.

**Exit**: no byte the kernel heap hands out holds data a previous owner of its
frame wrote, and the zeroing that ensures it runs under no lock another CPU
waits on.
