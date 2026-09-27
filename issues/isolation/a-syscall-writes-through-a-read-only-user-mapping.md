---
status: assigned
kind: defect
opened: 2026-09-26
---

# A syscall writes through a read-only user mapping

Found in the review of the logging track (PR #527) and fixed on that branch;
held by it, and deleted by whichever change first lands after it on `main`.

**On `main`, a kernel copy into user memory checks that the page is present,
never that the process may store to it.** `user_ptr::window` and
`user_ptr::copy_out` translate through `AddressSpace::translate`, which walks to
any present leaf, and then write through the direct map, where the leaf's
`WRITE` bit does not apply. So any syscall that fills a caller's buffer —
`read`, `fstat`, `sched_info`, `process_stats` — writes a page mapped
`Prot::Read` or `Prot::ReadExec`:

- an `mmap(PROT_READ)` region, which the process then reads back rewritten;
- its own `.text`;
- a shared library's `.text`, which `LibMemory::Shared` maps from one cached
  image into every process that loads it — so the write is **cross-process**;
- on the branch, the clock page, one frame every address space maps.

`tests/toyos-rust-tests/src/bin/abuse_readonly_copyout.rs` is the gate. With
the branch's `translate_writable` and the `Access` it threads through
`user_ptr` reverted as one patch, it reds: `read wrote into a read-only mmap`,
exit 1. Put back, it is green, exit 0. The code that patch reverts to is
`main`'s byte for byte.

The oracle is the MMU's own rule (Intel SDM vol. 3A §4.6.1): a ring 3 store
needs `R/W` and `U/S` set at every level of the walk under `CR0.WP`, which is
what `translate_writable` checks.
