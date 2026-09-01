---
status: open
kind: track
opened: 2026-09-01
---

# Two lifetime edges the loom models stop short of: the probe node's victim-retire, and nested crash entry

The rule that a model must import its subject rather than transliterate it is
already this tree's, and already enforced. `toyos-sched/loom/src/lib.rs` pulls
every scheduler source in by `#[path]` (`:26-49`), and its header at `:6-8` says
why: "loom explores the interleavings of the *real* primitives, not of a
re-implementation — a re-implementation is exactly the divergence risk this
crate is meant to remove." What is owed is not the principle. It is two edges.

**The steal probe's node — the model exists and stops one edge short.**
`toyos-sched/loom/tests/loom_mailbox.rs:186`,
`steal_probe_node_is_never_double_linked`, already drives the real
`MailboxNode`/`MailboxConsumer` through claim, post, pop and repost, and asserts
exactly-once consumption plus `!in_flight()` at the end.
`loom_retire.rs:163` models node reuse under a racing migration. Neither crosses
what `issues/kernel/steal-probe-node-dies-with-its-victim.md` is actually about:
the victim being **retired** while a probe is outstanding, and the node's
**drop** on that path. The existing model's victim always drains. Extend it —
do not write a second model, and do not restate the principle the crate header
already states.

**Crash entry under preemption — no model at all.** Real preemption state is two
words, not one: `kernel/src/preempt.rs` carries a `preempt_count` and a separate
`need_resched`, and `enable_no_resched` (`:55`) exists precisely because dropping
the count without polling is a distinct transition — its own comment at `:53`
says so. Crash entry assumes an exact invariant over that pair, and
`kernel-loom` models the panic-console snapshot latch and nothing about it. A
Boolean model would erase the nesting and pass. Import the counter transition,
model interrupt and nested-crash schedules, and observe fallback-channel
completion; the negative control restores preemptible crash entry.

Both crates already have their negative controls wired in CI, so a model that
cannot be made to fail is refused on arrival.
