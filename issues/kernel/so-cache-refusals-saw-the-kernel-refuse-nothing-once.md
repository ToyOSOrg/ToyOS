---
status: open
kind: defect
opened: 2026-09-03
---

# `so_cache_refusals` saw the kernel refuse nothing, once, on CI

`ci` run 33756442944, `guest (5)`, 2026-09-03, on `w5b15-ready`, whose diff
touches no loader or cache file; `ALONE so_cache_refusals: GREEN, and it was
alone both times` in the same job.

The verdict: no "byte budget; refused" line — the kernel refused nothing:
twelve 2 MiB images entered a cache whose test budget refuses at the second.

Owed: a mechanism. Nobody has one.

**Exit condition.** The cause of the missing refusal is fixed, shown against
`so_cache_refusals` and the `so-cache-tiny` budget it arms, as both stand at
`1808fb8d`, restored and green on CI's KVM `guest` shards. Owner: orchestrator.
