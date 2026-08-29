---
status: open
kind: finding
opened: 2026-08-29
---

# The xHCI sim cannot stage a device pulled mid-warm-reset

Found by #342's review, probe-proven: `FakePort` has no physical-presence
field — `detach()` is `if raw & CCS != 0`, so once a failed bus reset clears
CCS a detach is a silent no-op and the warm completion re-raises CCS|CSC
(PORTSC read back 0x002a0e03 after an explicit mid-warm detach). The exact
artifact `acknowledging_connect` swallows is therefore untestable in the
oracle, and the re-raise keys on `matches!(self.behaviour, FailsTheBusReset)`
— a scenario label inside a register model, the construct that lets a future
scenario be "fixed" by naming it. Exit: give `FakePort` a `present: bool` set
by attach/detach with CCS derived from it; the label test then goes. Second
copy in the same area: sim/src/driver.rs:396-411 hand-mirrors device::begin's
acknowledge with nothing tying the copies together.
