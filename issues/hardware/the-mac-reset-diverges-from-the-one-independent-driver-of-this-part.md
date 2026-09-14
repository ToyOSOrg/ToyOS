---
status: open
kind: defect
opened: 2026-09-14
---

# The MAC reset is issued without the PHY semaphore and read back a microsecond later, and the one independent driver of this part does neither

`toyos-i219/src/lib.rs`'s `open` writes `CTRL.RST` (`kernel`-free, userland
path) and then polls `CTRL` for the bit to clear, having waited §10.2.2.1's
microsecond. Both halves diverge from the only independent implementation of
this same silicon that can be read, and the divergence is on the path every
boot takes: the MAC reset is issued whatever happens to the PHY afterwards.

Linux's `e1000_reset_hw_ich8lan` (`drivers/net/ethernet/intel/e1000e/ich8lan.c`,
which covers I217/I218/I219 as `e1000_pch_lpt` and newer):

```c
	ret_val = e1000_acquire_swflag_ich8lan(hw);
	e_dbg("Issuing a global reset to ich8lan\n");
	ew32(CTRL, (ctrl | E1000_CTRL_RST));
	/* cannot issue a flush here because it hangs the hardware */
	msleep(20);
```

Two statements in four lines:

1. **The MDIO semaphore is held across the reset.** This driver takes it
   afterwards, inside `phy::bring_up`, so the reset is issued while the
   Management Engine may be mid-transaction on the PHY the reset reaches.
2. **A register read immediately after `CTRL.RST` hangs the hardware.** `e1e_flush`
   is a read of `STATUS`; the comment says it may not be made there, and the
   driver sleeps 20 ms instead of reading anything. This driver waits 1 µs and
   then polls `CTRL` — a read — for up to 100 ms.

Neither can be checked against the I219's own datasheet, which is AES-128
encrypted on this machine and unreadable. The 82574 document this driver cites
says only that the bit is self-clearing and that designers "must wait
approximately 1 µs" before checking it; it is not a document about the part in
the PCH.

## Why this is filed rather than fixed

It is not what the `i219-phy` investigation was about, and the evidence points
the other way for now: metal run 51 issued exactly this reset with the PHY
sequence absent and the machine was healthy — `Boot: complete (3199ms)`, ssh
back in 84 s, readback written, stick survived. So this reset, read-back and
all, has been observed to work once on this machine. That is one boot, against
a comment in an implementation that has run on millions.

## What would close it

Either the divergence is removed — the semaphore taken before `CTRL.RST` and
the post-reset settle made a wait rather than a poll — or it is recorded as a
deliberate difference with the evidence that the 82574's wording governs this
part after all. An answer that rests on run 51 alone is one boot, and the
failure mode it would be wrong about is the machine not coming back.
