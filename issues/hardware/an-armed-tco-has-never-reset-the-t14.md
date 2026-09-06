---
status: open
kind: defect
opened: 2026-09-06
---

# An armed TCO has never reset the T14, at any bound

Every metal run so far has needed a hand on the power button. Runs 3 and 4 both
armed the chipset watchdog and both had to be power-cycled by the owner (owner
ruling, 2026-09-06); no earlier claim that the machine reset itself survives —
run 3's 465 s down interval was the owner, not the timer.

Run 4's `loader.log` is the strongest evidence there is, because it is the arm
and the read-back in one line:

```
watchdog: 8086:a0a3 TCO at 0x400 TCO_TMR=8 armed for 9600ms, and the kernel takes it over
```

So the block was found, the port was `toyos-tco`'s Tiger Lake row, `TCO_TMR`
took the value, and `TCO1_CNT` read back with `TCO_TMR_HLT` **clear** — the
loader refuses by name when it does not, and did not refuse. The kernel then ran
to its `control_regs` line and stopped
(`issues/kernel/the-t14-stops-inside-percpu-init-bsp.md`), never reaching its
own arm, so nothing fed or re-armed the timer. It sat 14+ minutes at a 9.6 s
bound and the machine was still showing the same panel.

That leaves the reset gate, and it is **not in the TCO block**: the second
expiry reboots only while the PCH's `NO_REBOOT` is clear, and that bit lives in
the power-management controller's own space at an address no source in this tree
cites. `toyos-tco::RESET_GATE_IS_OUTSIDE_THIS_BLOCK` is that sentence at the
site. This tree will not write a guessed MMIO offset on the owner's laptop to
find out — that is the same sin as guessing the I/O port, in a space where the
wrong write is worse. Ubuntu on this machine exposes no
`/sys/class/watchdog` and the firmware publishes no `WDAT`, which is consistent
with a gate the firmware sets and does not advertise.

What the tree could decide, it has: the loader now prints `TCO_RLD`, `TCO_TMR`,
`TCO1_CNT`, `TCO1_STS` and `TCO2_STS` as whole words right after arming, then
stalls one 600 ms tick and reads `TCO_RLD` again. That second read is the
question no single read answers — *does this timer count at all* — and it splits
the remaining candidates cleanly:

- **`TCO_RLD` did not move**: the timer is not running, and no bound it was
  armed with could ever expire. The halt bit read back clear, so the cause is
  elsewhere in the block, and the printed words are the evidence.
- **`TCO_RLD` moved**: the timer counts and the reset is gated downstream —
  `NO_REBOOT`, or a first-expiry SMI the firmware services. `TCO1_STS` on the
  *next* boot then separates those two: a latched first-timeout bit means the
  expiry happened and only the reboot was suppressed.

q35 is the positive control and prints the moved branch
(`TCO_RLD went 0x0007 -> 0x0006 over 700ms`), asserted by
`loader_watchdog_arms`. **The T14 is the judge for which branch it prints.**

**Exit condition**: one metal run's `loader.log` carrying the two new
`watchdog:` lines, and — if the timer counts — the datasheet section naming
`NO_REBOOT`'s address on Tiger Lake-LP, cited in `toyos-tco` before anything
writes it.
