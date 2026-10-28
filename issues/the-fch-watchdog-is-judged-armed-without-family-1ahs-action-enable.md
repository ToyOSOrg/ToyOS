---
status: open
kind: finding
opened: 2026-10-28
---

# The FCH watchdog is judged armed without Family 1Ah's action enable

On Family 1Ah, `WatchdogControl` bit 4, `watchdogactionen`, is set by MP1 and
clears on entry to a power-saving mode; while it is 0 an expiry sets
`WatchdogFired` and resets nothing (58264 §12.3.6). The same bit is reserved on
Family 17h, so `toyos_tco::fch::arm` and the kernel's take-over keep it as read
and the loader reports it as `action_en=`, but nothing judges it. A Family 1Ah
machine that reads 0 there is reported armed with no reset behind its bound.
No Family 1Ah machine is in reach.

**Exit**: on a Family 1Ah FCH, an arm that reads `watchdogactionen` 0 is
refused, and a host model case holds it.
