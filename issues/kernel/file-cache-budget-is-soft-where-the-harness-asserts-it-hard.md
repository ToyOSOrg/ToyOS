---
status: open
kind: defect
opened: 2026-08-28
---

# `cache_eviction` red on CI: the file-cache budget is soft where the harness asserts it hard

One measurement: ci.yml run 33159606357, guest (3), PR #310 (a tracker-only
diff), parallel phase — `FAIL cache_eviction: file cache: 65 entries resident
against a 64 bound after 1280 evictions — the bound does not hold`. Green
`ALONE` in the same session; the harness itself printed "failed once and
passed once. That is a rate and not a classification."

The two sides genuinely disagree, and the sample is honest:

- `kernel/src/file_cache.rs` `evict_if_needed` stops when a full CLOCK
  revolution finds nothing it may take — eviction never takes a dirty page,
  and the code says the escape aloud: "the only bound on dirty pages is the
  writer's un-flushed working set." A moment where the resident set is
  all-dirty leaves `cached_pages` above `max_pages`, and the turnover line
  then truthfully prints `65/64`.
- `tests/toyos.rs:10031` asserts `resident > budget` red at every sample —
  a hard bound the kernel does not promise. Under a loaded parallel phase
  the write-back drain (`iod`) lags exactly when the tests around it are
  loudest, which is why the overage shows beside other guests and not alone.

Per the harness rule, a bound is asserted against the derivation and a red is
only the outcome that is neither the answer nor the declared degradation.
The fix is at one of the two sites, owner's pick: the turnover line reports
its dirty count and the harness admits an overage only while every resident
page is dirty — or the budget becomes hard by writing back under pressure,
which is a kernel behaviour change, not a test edit.
