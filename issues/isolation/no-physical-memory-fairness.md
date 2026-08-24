---
status: open
kind: defect
opened: 2026-07-30
---

# No physical memory fairness

Any process can allocate unbounded physical memory until the system runs out.
No per-process limits, no memory pressure signals, no OOM killer. A single
misbehaving process starves everything.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling: isolation
findings are security-adjacent and are promoted, never folded). One process
starving every other of physical memory is a denial of service across the
isolation boundary that needs no privilege and nothing crafted — it is real
today, not a possibility. Owed by whoever builds per-process physical
accounting; nothing in the tree measures or refuses it now.
