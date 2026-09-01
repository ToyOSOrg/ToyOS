---
status: open
kind: track
opened: 2026-09-01
---

# Two lifetime races have no model, and a transliterated model would validate itself

A loom model that re-types its subject by hand proves that the *transcription* is
correct under interleaving. It says nothing about the tree. The rule is: import
the real factored primitive, or do not claim the model as an oracle. Two subjects
need one built that way.

**The steal probe's node.** `issues/kernel/steal-probe-node-dies-with-its-victim.md`.
An outstanding probe suppresses reposting, a stopped victim can cost half the
pulls, and completion of the consumer is what clears the flag —
`toyos-sched/src/mailbox.rs` holds `in_flight` as an `AtomicBool` swapped at
publish (`:154`, `:295`) and stored false at consume and at drop (`:202`, `:379`).
The model must cross publish, consume, victim-retire, drop and repost, and count
exactly-once reclamation. Its negative control is restoring victim-owned lifetime.

**Crash entry under preemption.** `issues/panic-path/crash-report-preemption-untested.md`.
Real preemption state is two words, not one — `kernel/src/preempt.rs` carries a
`preempt_count` and a separate `need_resched`, and `enable_no_resched` (`:55`)
exists precisely because dropping the count without polling is a distinct
transition. Crash entry assumes an exact invariant over that pair. A Boolean model
erases the nesting and would pass. Import the counter transition, model interrupt
and nested-crash schedules, and observe fallback-channel completion; the negative
control restores preemptible crash entry.

**Where each lives.** The mailbox model belongs in the scheduler's own loom crate,
the crash model in `kernel-loom` — which today is one `lib.rs`. Both crates
already have negative controls wired in CI, so a model that cannot be made to
fail is refused on arrival.
