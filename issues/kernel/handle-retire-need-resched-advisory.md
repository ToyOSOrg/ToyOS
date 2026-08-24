---
status: open
kind: defect
opened: 2026-07-30
---

# `handle_retire`'s `need_resched` on a running target is a request the next pass may decline

Re-checked against `739af0c2` on 2026-08-24; the scheduler core moved to
`toyos-sched/` and the claim survived the move unchanged.

`SchedPass::preempt_if_due` (`toyos-sched/src/cpu.rs:1701`) fires on exactly two
things, and its own doc says so: quantum expiry, or an RT task in the band. The
condition is

```rust
// toyos-sched/src/cpu.rs:1715-1716
let rt_due = self.cpu.rq.has_rt() && !current.serves_rt_band() && !self.cpu.aged_grant;
let due = self.now >= self.cpu.quantum_end || rt_due;
```

and a merely-killed task matches neither. `kill_pending()` **is** consulted, but
only at `:1722` — after the preemption has already fired, to route the corpse to
the back of the dying list instead of to the fair queue. So the pass that
`need_resched` asked for can run, clear the request and resume the task, which
then dies only at the real quantum end.

That is what the retire protocol promises ("bounded by the quantum"), so it is
conformant rather than broken. What bounds the retirer while it waits is
`scheduler::retire_task`'s `GIVE_UP` — a 10 s `Tripwire`
(`kernel/src/scheduler.rs:970-974`), re-polled on a 50 ms `RECHECK` cadence —
and not the 100 quanta this entry used to name; that constant is derived
term-by-term in its own doc and is dominated by four xHCI pass prologues rather
than by anything the kill path does.

Adding `|| current.shared().kill_pending()` to the `due` expression above would
make the request mean what it says, for one atomic load per pass. It wants a sim
test beside it: `toyos-sched/sim` already gates the arm below it
(`a_killed_task_that_expires_its_quantum_goes_back_to_the_dying_list`), and the
new arm's negative control is that same shape one condition earlier.
