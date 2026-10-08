---
status: open
kind: defect
opened: 2026-10-08
---

# A refused syscall writes a log record per call, and nothing bounds the caller

A refusal the kernel answers with an error is also a kernel log record, once
per call, at every one of these sites, and the caller chooses how often:

- `sys_dlopen` (`kernel/src/syscall/vm.rs`): a path that does not open
  (`dlopen: <path>: <error>`), a cached image whose file changed, an image
  `elf::load_shared_lib` refuses, no virtual address space left, and a TLS
  reference that leaves its module's segment;
- `elf::cache_loaded_lib` (`kernel/src/elf/cache.rs`): a load that would take
  the shared-object cache past its budget;
- a refused spawn (`kernel/src/loader/mod.rs`): every `spawn: <path>: …`
  record, one for each reason a file can be refused.

`dlopen` of a missing path in a loop, or a spawn of a file that is not an
executable, is a record per syscall from any program, into a log every
program shares: `logkeeper` keeps sixteen megabytes of a boot
(`issues/a-t14-boot-that-outlogs-its-retention-loses-its-middle-and-the-rows-whose-lines-sat-there.md`),
and what a storm of these pushes out is the middle of everybody else's.

`kernel/src/loader/mod.rs`'s header states the refusal's record as the
contract, and `kernel/src/process.rs`'s that nothing a process writes is
rate-limited. The kernel holds no limiter: its one user, a thread's end, was
deleted with the record it limited. `logkeeper` limits what a program writes
through its own ring (`userland/logkeeper/src/origin.rs`), and these are the
kernel's records, which that limit does not reach.

Not measured: no test loops a refused call and reads what the log grew by.
The sites are older than the contract that now names them.

Owner: the syscall layer, `kernel/src/syscall/`, with the loader's header.

## Exit condition

The log's volume from refusals does not grow with how often a program asks.
One of two shapes, decided by which a reader of a failed boot needs:

- a refusal the caller is told by its error is not also a record: the error
  names the reason, and the program that cares says it in its own ring, under
  `logkeeper`'s limit; or
- the kernel counts a process's refusals and says the count once, in that
  process's `exit:` record.

Either way the two headers say which, and a guest test has one process make a
refused `dlopen` and a refused spawn a thousand times each and finds the
number of kernel records that name it the same as after one.
