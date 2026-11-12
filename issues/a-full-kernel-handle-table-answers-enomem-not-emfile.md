---
status: open
kind: defect
opened: 2026-10-21
---

# A full kernel handle table answers ENOMEM, not EMFILE

The kernel answers a full handle table `ResourceExhausted`
(`kernel/src/object/handle.rs`, `HandleError::TableFull`), the same word it
answers for no memory, so the SDK can read it only as one failure
(`sdk/toyos/src/failure.rs`, `Failure::OutOfMemory`): libc answers that
`ENOMEM` and std `OutOfMemory`. So an `open`, `pipe` or
`/dev/null` that a full kernel table refuses says `ENOMEM`, where POSIX has
`EMFILE` for a process that holds as many descriptors as it may. The kernel's
table is the binding limit for descriptors that are kernel handles: it holds
as many slots as `toyos::fd::MAX_FDS`, and the process's own handles take some
of them. Corpus case `212_full_table` asserts the `ENOMEM`.

A word of the kernel's own for a full table would let libc say `EMFILE`. The
owner ruled no kernel change for POSIX alone, so this waits on a reason the
kernel's own callers have to tell the two apart.

- **Owed on metal:** the T14's `testcases-watchdog` boot at main after #1227
  (one error table), which no guest row runs: `main` must show it green. The
  `testcases` members, `212_full_table` printing
  `kernel handles ran out first with ENOMEM: yes` and `206_libc_refusals`
  printing `sysconf _SC_ARG_MAX: 2023395` among them, are the guest row
  `testcases_members_exit_0`'s.

**Exit**: a full handle table and no memory are two answers at the kernel
boundary, the SDK reads the first as `Failure::TooManyDescriptors`, which
libc already answers `EMFILE`, and `212_full_table` expects it.
