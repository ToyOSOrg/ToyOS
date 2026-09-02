---
status: open
kind: defect
opened: 2026-08-27
---

# A steal probe posted into a CPU that then stops takes its thief's pull half with it

`SchedPass::post_steal_probe` rides one `MailboxNode` per CPU — the thief's own
`CpuSched::steal_probe` — and that node's `in_flight` flag is cleared by the
*victim's* consumer, in `MailboxConsumer::consume`, when the victim drains the
message. A victim that takes no further passes never drains it, so the flag
stays raised for the rest of the boot and `steal_probe.claim()` answers `None`
every time the thief asks again.

The thief is then a CPU that can never pull. Worse than that, it does not know:
`SchedPass::probe_still_owed` answers `false` while a probe is outstanding, so
the thief halts against a published surplus it can see and would otherwise have
gone and taken. One CPU that stops therefore costs a second CPU its half of the
balance path, permanently, and nothing in the machine says so.

`CpuHandle::answering` narrows this and cannot close it. A victim that has
*already* gone quiet is no longer chosen — that is what the staleness test
does — but a victim chosen while it was answering and stopped afterwards leaves
the node exactly where it was. The window is one probe wide and the damage is
for the boot.

Nor can the node be reclaimed. It may still be linked into the stopped CPU's
queue, and re-posting a linked node is what invariant N1 in
`toyos-sched/src/mailbox.rs` exists to forbid: the consumer would walk a node
whose `next` had been rewritten under it. Whatever closes this has to be a
second node, a probe that is not node-shaped, or a way for a CPU to disown its
whole mailbox — none of which is a change to `post_steal_probe`.

Found while closing the placement half of the freeze family (the shed-core
work), which is the only reason the state is reachable in the first place: a CPU
that stops taking passes is what every part of this rests on, and why one does
is still open (`issues/kernel/spawned-process-never-starts.md`).

## It reproduces in a second, on the real primitive

`toyos-sched/loom/tests/loom_mailbox.rs`'s `steal_probe_model` drives the real
`MailboxNode`, `MailboxProducer` and `MailboxConsumer` through claim, post, pop
and repost. The `victim-retires-mid-probe` feature makes the victim's last pass
its last, and loom finds the schedule:

```
$ TOYOS_LOOM_RAW=1 cargo test -p toyos-sched-loom \
    --features victim-retires-mid-probe --test loom_mailbox
test a_probe_outstanding_when_its_victim_retires_is_never_reclaimed ... FAILED
assertion `left == right` failed: the victim retired with a probe still linked
in its queue, so the node stays claimed for the rest of the boot and every
later `claim()` by its thief answers `None`
  left: 1
 right: 0
```

Without the feature the same model is green (`steal_probe_node_is_never_double_linked`),
so what the red measures is the retirement and not the model. What it does not
measure is the width of the window: the model's thief posts unconditionally,
where `best_victim` posts only into a CPU `CpuHandle::answering` still admits.

**No drop edge is modelled, and an earlier draft of this entry claimed one.**
`MailboxConsumer` has no `Drop` impl — `toyos-sched/src/mailbox.rs` has two,
`MailboxNode:172` and `PostSlot:200` — so dropping the consumer runs no code,
and the count above already clears `in_flight` before any assertion. The line
that dropped it was deleted after it was measured bit-identical in both arms.

**The control and its feature are deleted by whatever closes this entry**, in
the same commit. Not flipped to green: the feature *defines* every post-join pop
as stranded, so no change to the mailbox can make that case pass. Its premise
goes away with the defect, and the deletion is the only exit.
