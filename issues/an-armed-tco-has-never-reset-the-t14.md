---
status: open
kind: defect
opened: 2026-09-06
---

# An armed TCO has never reset the T14, at any bound

Every metal run so far has needed a hand on the power button. Runs 3, 4 and 5
all armed the chipset watchdog and all were power-cycled by the owner (owner
ruling, 2026-09-06). No claim that this machine reset itself survives — run 3's
465 s down interval was the owner's hand, and so was run 5's.

## What run 5 read back

Run 5's `loader.log` is the first evidence with registers in it:

```
watchdog: 8086:a0a3 TCO at 0x400 TCO_TMR=8 armed for 9600ms, and the kernel takes it over
watchdog: read back TCO_RLD=0x0008 TCO_TMR=0x0008 TCO1_CNT=0x1000 TCO2_STS=0x0000
watchdog: TCO_RLD still reads 0x0008 after 700ms, ...
```

**`TCO1_CNT` came back `0x1000` from a write of `0x0000`**, and the loader
reported the machine armed anyway: it judged that register on `TCO_TMR_HLT`
alone, which is clear inside `0x1000`. That is now fixed — the register is
declared whole and judged whole.

## What the datasheet says the bits are

*Intel 500 Series Chipset Family On-Package PCH Datasheet, Volume 2 of 2*,
document 631120-002 rev 002 — Tiger Lake-LP is `8086:a0xx`, and the TCO block is
the 32-byte I/O range at `TCOBAR` in SMBus configuration space (D31:F4), which
is the row `toyos-tco` already had right.

- **`TCO1_CNT` bit 12 is `TCO_LOCK`** (§32.1.6): "Once this bit is set to 1, it
  can not be cleared by software writing a 0 to this bit location. A core-well
  reset is required." So `0x1000` is firmware's lock, surviving the loader's
  write exactly as documented — **it is the one bit a read-back may not be
  judged on**, and judging on it would have disarmed the machine for run 6.
- **`TCO1_CNT` bit 0 is `NO_REBOOT_MSUS`** (§32.1.6): "When set, the TCO timer
  will count down and generate the SMI# on the first timeout, but will not
  reboot on the second timeout." **It reads 0 on the T14.** So the no-reboot
  gate is *not* what is stopping this machine from resetting.

That last point corrects this file's own earlier claim, which said `NO_REBOOT`
lived in the PMC at an uncited address and could not be read. It is in the TCO
block on this generation, the loader has been reading it since run 4 without
knowing what it was, and it is clear. **The bit is not portable**: on the 100
Series (document 332691-003EN) `TCO1_CNT`'s low byte is reserved and the
no-reboot bit is elsewhere, so any row added for another generation owes its own
citation. `toyos-tco`'s module header says this.

## What is left

Two candidates, and the tree can no longer decide between them:

1. **The timer is not counting.** `TCO_RLD` read `0x0008` twice. But the 700 ms
   between those reads was barely one 600 ms tick, and a counter reloaded at an
   unknown phase inside a tick can legitimately still read its loaded value —
   so run 5's line claiming "not counting" **overstated its evidence**. The
   stall is now two ticks plus a margin, which one cannot argue with.
2. **The first expiry's SMI is serviced by firmware.** `SMI_EN.TCO_EN` (bit 13
   of the SMI control register at offset 30h from the PMC's BAR2, §4.2.5) routes
   the first timeout to an SMI. `TCO_LOCK` being set is exactly what prevents
   this tree from clearing `TCO_EN` — the datasheet's own words for bit 12 are
   that it "prevents writes from changing the TCO_EN bit". Nothing is programmed
   for this: the PMC's BAR2 is a base this tree does not compute, and the lock
   makes the write pointless on this machine anyway.

`TCO1_STS` bit 3, whose datasheet name is `TIMEOUT` and not the `TCO_TMR_STS`
other sources use (§32.1.4), separates them: it latches on the first expiry, so
a boot that stayed up with it set expired and only the reboot was suppressed.
The loader now prints it.

q35 is the positive control and prints the counting branch
(`TCO_RLD went 0x0007 -> 0x0005 over 1300ms`) with `no_reboot=0`, both asserted
by `loader_watchdog_arms`. **The T14 is the judge for which branch it prints.**

**Exit condition**: one metal run's `loader.log` carrying the two-tick read and
`TCO1_STS`, which decides between the two above; then whatever that names.
