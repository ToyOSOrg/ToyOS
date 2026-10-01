---
status: owner
kind: question
opened: 2026-10-01
---

# Whether a boot may starve the T14's watchdog is the owner's

The question: may one test boot on the T14 stop feeding the chipset's watchdog
on purpose, so that the chipset resets the laptop?

The watchdog is what will reset a frozen ToyOS on every machine (owner ruling),
and on the T14 it is the only reset that needs nothing of the kernel. Nobody has
seen it reset this laptop: runs 3 to 5 armed it, and each needed a hand on the
power button (`issues/hardware/an-armed-tco-has-never-reset-the-t14.md`).

- **What a yes risks**: the laptop resets, as it does at the end of every test
  boot, but by the chipset's timer rather than by ToyOS. What this laptop's
  firmware does after such a reset has never been seen. If the chipset does not
  reset it, the boot's own deadline ends it, as it ends the `deadlinewedge`
  boot in every run.
- **What a no stops**: the reading in
  `issues/hardware/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md`,
  and the T14 exits of that track and of stage 8 of
  `issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`,
  which need the same reset. `watchdogd` then ships to the T14 with its reset
  unproven there.

*Recommended: yes.*

**Exit**: the owner rules.
