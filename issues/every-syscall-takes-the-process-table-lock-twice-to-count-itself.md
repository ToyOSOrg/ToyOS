---
status: open
kind: defect
opened: 2026-10-12
---

# Every syscall takes the process table lock twice to count itself

`syscall_dispatch` (`kernel/src/syscall/dispatch.rs`) calls
`process::with_current_data` before the call, to bump `syscall_total` and
`syscall_counts`, and again after it, to add `syscall_total_ns`. Each one takes
`PROCESS_TABLE`'s lock, clones the thread's `Arc`, takes that thread's
`Lock<ThreadData>` and drops the `Arc` (`current_data` and `with_current_data`
in `kernel/src/process.rs`). That is twelve locked read-modify-writes on every
syscall, `SYS_GETPID` included: per call, two ticket acquisitions and their two
releases, and the `Arc`'s increment and decrement. It also takes the one
machine-wide table lock, which every thread of every process contends for, to
update a counter that belongs to one thread.

Counted from the code. Priced at the per-prefix cost the Ring 3 controls of PR
#866's scout measured, 15 TSC cycles on the T14 and 5–6 on the AMD bench
(https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119438569), the twelve
come to about 180 and 66 cycles. That is an estimate, not a measurement of
these sites.

**Exit**: a syscall that returns to its own thread takes no machine-wide lock and
no `Arc` reference to count itself, and `syscall_cost` is read on both benches
before and after.
