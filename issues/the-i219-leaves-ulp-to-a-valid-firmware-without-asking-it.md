---
status: open
kind: finding
opened: 2026-10-28
---

# The I219 leaves Ultra Low Power to a valid firmware without asking it

Where `FWSM` bit 15 reports the firmware valid, the Management Engine
configures the PHY's Ultra Low Power, and `toyos_i219` writes none of it
(`system/i219/driver/src/ulp.rs`). It also asks the engine for nothing: a
host driver on such a machine asks it to leave ULP through the MAC's
host-to-ME register at `0x5B50` and waits on `FWSM` for the engine's answer,
and this driver has neither. A PHY the engine left in ULP there stays out of
reach.

The T14 reports the firmware not valid on every logged boot, so no bench has
this shape.

**Exit**: a machine whose `FWSM` reports the firmware valid, and a bring-up on
it that asks the engine to disarm ULP and is read reaching the PHY; or the
part refused by name on such a machine.
