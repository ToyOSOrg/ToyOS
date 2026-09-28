---
status: open
kind: tooling
opened: 2026-09-28
---

# A settled thread join taking the table lock again is gated by nothing

`toyos_proclife::join::Join::ask` calls its `collect` only while the join is
unsettled, and `a_settled_join_does_not_take_the_table_again` holds that.
Whether `collect` is where the kernel takes `PROCESS_TABLE` is
`kernel/src/process.rs`'s `ask_join`, in no crate a host test compiles. PR
#564's round-4 review wrote it as
`let mut g = PROCESS_TABLE.lock(); join.ask(|| join::collect_zombie(g.as_mut().unwrap(), parent_pid, tid))`.
A settled join then takes the lock on every wake of its wait.

**Exit**: taking `PROCESS_TABLE` outside `ask_join`'s `collect` reds a host
test or a fast-tier test. Owner: orchestrator.

The syscall's answer-keeping is the same gap: `sys_thread_join`
(`kernel/src/syscall/proc.rs:154`) holds one `Join` across every ask, but
`a_join_asked_after_it_collected_keeps_its_answer` only replicates that shape
rather than calling the syscall, so a `Join::ask` built fresh per ask
(`join-per-ask`) stays host-green and reds only on the nightly guest
`fpu_isolation` (`thread_join failed`, `left: 18446744073709551614`).
