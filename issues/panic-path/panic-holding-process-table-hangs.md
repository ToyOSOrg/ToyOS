---
status: open
kind: defect
opened: 2026-07-30
---

# A panic while holding `PROCESS_TABLE` hangs the panicking CPU

`try_recover_from_panic` lands in `sched::driver::idle_loop`, whose
`reap_poisoned` takes that lock unconditionally every iteration, and the dead
thread never releases it. Pre-existing and unchanged by the panic-recovery fix; a
`try_lock` could not have saved it either, since a spinlock's `try_lock` fails
for its own holder too. The general shape — locks a dead thread can strand —
belongs to the capability-handles/ownership work.

**The VFS lock is the same shape**, and it was the one that bit first: a
`read_dir` over 32,769 files panicked inside `vfs::lock()`, and every later
filesystem operation on the machine spun on it. Measured after `889d611` — the
process was killed and the harness still got its end marker, because the test
runner's report path does not touch the VFS. That particular route is bounded
now (`issues/isolation/`), but the class is not: any panic under `vfs::lock()` still strands it,
and the allocator was only the worst instance because every context allocates.

**`sys_mmap` was another instance.** A `PROT_NONE` mmap of a length near
`u64::MAX` passed the old size guard, then overflowed under both the
process-data lock and the address-space lock. Reverting #543's fix onto
16d2e645 and running `mmap_prot` reproduces it: `DEADLOCK at
src/process.rs:807:25: 500M spins, ticket=19 now=18`, both locks stranded by
the one panic. #543 moves the check ahead of both locks; the class this issue
tracks is unchanged.
