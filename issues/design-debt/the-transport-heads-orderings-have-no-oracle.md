---
status: open
kind: tooling
opened: 2026-09-27
---

# The transport head's orderings have no oracle

A consumer stores its head `Release` after it has loaded the entries below it,
and a producer loads the head `Acquire` before it writes over them
(`toyos-blockring/transport/src/queue.rs`, `Consumer::release` and `Producer::space`).
No test reds if either is `Relaxed`: what the pair forbids is load buffering —
a consumer's load of an entry reading the producer's later overwrite — which
loom does not model, and every other test runs both ends on one thread. The
tail's edge has its control (`publish-relaxed`); the head's has none.

**Exit condition.** A control that relaxes the head's two orderings, and a
model or a run on a weakly ordered CPU that goes red under it.
