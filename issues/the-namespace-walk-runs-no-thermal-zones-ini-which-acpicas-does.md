---
status: open
kind: finding
opened: 2026-10-12
---

# The namespace walk runs no thermal zone's `_INI`, which ACPICA's does

The server's initialisation (`system/acpiserver/src/init.rs`) examines a
Device or Processor object by its `_STA` and runs its `_INI`, and passes a
ThermalZone by: a thermal zone's `_STA` is not asked and its `_INI` is never
run, and nothing below one is examined. ACPICA 20260408's walk examines a
thermal zone as it does a device (`AcpiNsFindIniMethods` and
`AcpiNsInitOneDevice`, `source/components/namespace/nsinit.c`), and #875
made the walk ACPICA's in which devices it examines. Neither bench's
thermal zone names an `_STA` or an `_INI`, read outside the tree on both
dumps (#875), so neither machine's initialisation differs for it.
