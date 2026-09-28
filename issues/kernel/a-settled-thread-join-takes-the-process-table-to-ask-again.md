---
status: open
kind: defect
opened: 2026-09-28
---

# A settled thread join takes the process table to ask again

`sys_thread_join` (`kernel/src/syscall/proc.rs`) asks through
`process::ask_join`, which locks `PROCESS_TABLE` before `Join::ask` looks at
whether an earlier ask settled. A join whose wait's predicate collected the
zombie then asks once more at the top of its loop, so it takes the machine-wide
table lock for an answer it already holds. Found by PR #564's round-2 review.

**Exit**: a settled join answers without taking `PROCESS_TABLE`. Owner:
orchestrator.
