---
status: open
kind: finding
opened: 2026-09-27
---

# `i8042_health_cadence` counted three counter lines for two keystrokes once on main's nightly

Main's nightly at 1ce71831 (run 36290616312), one guest shard, wide:
`FAIL i8042_health_cadence: two keystrokes three seconds apart, 3 counter
lines — the report is on a timer rather than on the pin`. `ALONE
i8042_health_cadence: GREEN` twice.
`cargo run -- --known-red i8042_health_cadence` answers NO.

**Exit**: the red run's three counter lines matched to the edges that
produced them, or a rate with enough runs to call it gone.
