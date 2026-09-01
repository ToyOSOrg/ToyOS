---
status: open
kind: track
opened: 2026-09-01
---

# No test can read this machine's mappings, so a two-ledger inventory cannot be shown to agree

`issues/kernel/one-mapping-is-written-in-two-ledgers.md` is the subject: address
space regions live in `AddressSpace.regions` and process mmap metadata lives in
`ProcessData.mmap_regions`, and nothing proves the two stay identical across
every return edge — including the failure edges, which is where a rollback that
updates one ledger and not the other would show.

**What to build.** A test-only mapping inventory readable from *both* ledgers,
plus the commit/rollback transition factored out as a pure state machine and
exercised at each failure point. Two artifacts, and both are needed: the pure
model finds a transition that can diverge, and the inventory proves the real
kernel took it.

**The instrument can hide what it measures.** A test-only inventory that takes a
lock to read serializes the very race the divergence needs. So the model, not the
inventory, is where interleavings are searched; the inventory is sampled only at
quiescent barriers, where it answers "did they agree" and nothing about when.

**This is an instrument, not the exit.** The eventual answer is consolidation —
one ledger — and this track exists because nobody can currently show that
consolidating changed nothing. Do not let the model become the fix.
