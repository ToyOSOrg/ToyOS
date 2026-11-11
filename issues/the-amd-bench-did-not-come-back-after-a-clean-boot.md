---
status: open
kind: finding
opened: 2026-11-11
---

# The AMD bench did not come back after a clean boot

On 2026-11-05 the AMD bench stayed down after a ToyOS boot that had stopped
cleanly, until its power was cycled. Seen once, in the run
`amd-q-memcpy11-amd-1`, on an image built at `b76abc3bd`: a measurement-only
revision of the AMD memcpy work, from before #1196.

**What the bench's log partition holds**, saved after the power cycle:

1. The boot started at 17:07:32 UTC and ran its jobs. Its last job asked the
   reboot, the stop took 17 of 17 userland threads, and the kernel said
   `Rebooting.` at 19.618 s.
2. The next loader pass ran. It read the last boot's record as `DONE`, said
   the chain ends there, and its last line is `Loader log: the last boot is
   accounted for, so this pass resets the machine`.
3. Nothing follows that line. The pass writes it, closes its log, and only
   then asks firmware for a warm reset.

**What the runner shows.** The machine never answered again: the runner gave
up at 17:26:35 UTC, after the 1140 s it waits. The next four queued boots
ended on the same unanswered connection, with nothing flashed.

Unmeasured: whether the loader reached its reset call, whether firmware came
back from it, and whether the default entry, the bench's own system, started
and hung. The log was closed before the call, and the panel was not
photographed.

**That pass is gone.** #1196 deleted the loader pass that read a record and
ended in a reset. On main a pass that finds a clean stop's mark boots a
kernel, and the loader resets only from its panic path and from a refused
`ExitBootServices`. What is left to watch for is this bench not coming back
from a reset after a clean stop, which is now the kernel's own, as in
`issues/a-t14-boot-said-rebooting-and-the-host-never-came-back.md`.

Evidence, masked:
https://github.com/ToyOSOrg/evidence/tree/7899c5108417a40addb5d4d68e4825d62ca70260

**Exit**: a sighting on this bench of a reset it did not come back from,
whose record names the side that hung; or the owner's word that one sighting
on a deleted pass is owed nothing.
