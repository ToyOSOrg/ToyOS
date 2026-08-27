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
