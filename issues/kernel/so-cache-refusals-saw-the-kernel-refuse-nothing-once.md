---
status: expected-red
kind: defect
opened: 2026-09-03
---

# `so_cache_refusals` saw the kernel refuse nothing, once, on CI

`ci` run 33756442944, `guest (5)`, 2026-09-03, on `w5b15-ready`, whose diff
touches no loader or cache file; `ALONE so_cache_refusals: GREEN, and it was
alone both times` in the same job.

The verdict: no "byte budget; refused" line — the kernel refused nothing:
twelve 2 MiB images entered a cache whose test budget refuses at the second.

Owed: a mechanism. Nobody has one. `src/redlist.rs` quarantines the name until
then.
