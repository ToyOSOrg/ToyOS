---
status: open
kind: track
opened: 2026-09-01
---

# A process's memory is a byte total that reads zero under contention, so no plateau can be trusted

`issues/build/std-leaks-a-thread-stack-per-spawn.md` is the immediate consumer:
ToyOS std allocates a 2 MiB stack per spawn and `join` performs the join syscall
and nothing else. Deciding whether the mapping and its frames come back means
reading a *plateau* — a sequence that climbs and returns — and that is what this
tree cannot do honestly today.

**Three things already exist, and this track is the difference between them and
a plateau.**

`SYS_SYSINFO` carries a live per-process byte total: `demand_pages + mmap_regions
+ dynamic_tls_blocks + loaded_libs`, summed at `kernel/src/arch/syscall/machine.rs:146-156`
into the entry's `memory` word. A userland `mmap` becomes an `mmap_regions`
entry, so a leaked thread stack is inside that number. **But the sum is taken
under `try_lock`, and the failure arm writes `0`** (`machine.rs:155-157`). A
plateau reads a sequence; a spurious zero in that sequence is indistinguishable
from a release, and it appears exactly when the process is busy — which is when
a spawn/join loop is running.

`ProcessStats` (`SYS_PROCESS_STATS`) carries `alloc_count`, `peak_memory` and
the two fault counts. Subtracting frees from allocations would not give a live
count even if `free_count` were exported, and it is not (`kernel/src/process.rs:502`,
absent from the ABI struct) — **the two counters count different populations.**
`alloc_count` is bumped at four sites including the demand-page fill
(`kernel/src/arch/syscall/vm.rs:117`, `:142`, `:374`, `kernel/src/process.rs:1432`);
`free_count` at two, `munmap` and TLS teardown (`vm.rs:161`, `process.rs:189`).
Demand-page release is never counted at all — `demand_pages` is cleared wholesale
at teardown (`process.rs:965`) — so the difference folds in every demand fill ever
taken and climbs forever. Exporting one field is the cheap fix that yields a
number which never returns. `peak_memory` is a high-water mark, which means a
*fix* is invisible in it: a leak that stops leaking leaves the peak where it was.

`tests/toyos-rust-tests/src/bin/abuse_tls_alloc.rs` has the harness shape. It
runs raw `thread_spawn`/`thread_join` over an explicitly `mmap`ed stack, and its
comment at `:57-59` names this exact leak as the reason it avoids
`std::thread::spawn`. What it does not do is read a number.

**So what is missing is narrow and worth building.** A test-only per-process
*count* of live mappings and resident frames — counts, not bytes — read without
`try_lock`, so a sample is either a number or an explicit refusal and never a
silent zero. Then bounded spawn/join plateaus driven through the real std path.

**The counter owes two things.** It must not allocate per mapping, and it must
not retain the objects it counts. Both are proved by comparing its total against
a quiescent page-table and PMM walk at the start and at the end.

**Half the defect is not the joined half.** A `JoinHandle` dropped without `join`
detaches, and the record's exit condition currently closes only the joined path.
The plateau must cover both.
