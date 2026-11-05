---
status: open
kind: track
opened: 2026-11-05
---

# The handle lifecycle has no adversarial suite

From the external capability review of 2026-08-20, which the owner adopted:

- **The adversarial handle-lifecycle suite**: stale-handle behaviour, rights
  reduction and duplication, cross-process transfer, table exhaustion,
  teardown with references in flight, and every path where a numeric
  identifier might become authority. Measure it against what
  `abuse_handle_table`, `handle_lifetime` and the census arm already cover,
  and make the gaps tests. `handle_kill_policy` is deleted;
  `issues/handle-kill-policy-census-grew-one-sharedmem-on-two-nightlies.md`
  records the commit that restores it.
- **Zero-handle hooks stay mechanically constrained**: the drain sites' "no
  hook may take a sleep lock" doc constraint wants enforcement the compiler or
  an assert can see.

One known inconsistency, unobservable today: three device-driving arms demand
three different rights on the same claim handle (`WRITE`, `READ` and `NONE`).
No claim handle can carry a narrower set, because narrowing needs `DUP` and a
claim has none, so the three are the same test until a claim can be narrowed.

**Exit**: the suite's tests exist for every case above, each red on the
mutation it names, and the hook constraint fails a build or an assert.
