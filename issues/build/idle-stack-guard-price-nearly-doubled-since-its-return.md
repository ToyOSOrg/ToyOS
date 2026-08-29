---
status: open
kind: finding
opened: 2026-08-29
---

# `idle_stack_guard`'s CI price nearly doubled since its 2026-08-21 return

Returned to Fast on 2026-08-21 at 5,049 ms; the merged profile of run
33241967580 (main 48437ca4) prices it 9,601 ms — +90%, far past the 1.28x
p10-p90 shard spread that explains ordinary variance. `dump_nmi_probe` grew
+29% over the same window (6,284 -> 8,098 ms). Either the boots these ride got
genuinely slower — a real kernel-cost regression nobody measured on purpose —
or the shard fleet's pricing shifted; the two hypotheses separate on a bisect
of the boot's own timestamps across the window's landings. Noticed while
relegating the straddler batch; the relegation hides the symptom from the
per-PR gate, which is exactly why the growth is recorded here.
