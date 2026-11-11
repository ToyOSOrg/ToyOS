---
status: open
kind: finding
severity: [hang]
opened: 2026-11-11
---

# The AMD bench did not come back from the reset after a clean boot

On 2026-11-05 the AMD bench stayed down after a ToyOS boot that had stopped
cleanly, until its power was cycled. Seen once.

What the orchestrator reported of it:

1. The ToyOS boot ran and stopped cleanly; the log saved from it shows
   ToyOS's side finished.
2. The next loader pass read the clean stop's record and reset the machine
   to the firmware's default entry.
3. The machine hung in that reset or after it, and never reached the host.

The saved log is the orchestrator's and is not published; this record was
written from that report, not from the log.

Unmeasured: whether the reset was written, whether the firmware came back
from it, and whether the default entry started and hung. Nothing ToyOS keeps
says which, as in
`issues/a-t14-boot-said-rebooting-and-the-host-never-came-back.md`.

**Exit**: a second sighting whose record says which of the three it was, or
the cause named and fixed.
