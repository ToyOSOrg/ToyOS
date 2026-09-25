---
status: open
kind: defect
opened: 2026-09-25
---

# A fault before the kernel's IDT vectors into the firmware's, and says nothing

From `_start` until `percpu::init_bsp` loads the kernel's IDT (`kernel/src/main.rs`),
the table in `IDTR` is the one firmware left. A CPU exception in that window
(`pat::init`, `panic_console::arm`'s walk of the boot map, `mm::init`, the ACPI
parse) vectors into firmware code, which on EDK2 dead-loops; no panic runs, so
neither the panel nor the black box nor serial carries anything.

What a machine shows today is location, not cause: the handoff squares
(`toyos-bootmap/src/mark.rs`) say the kernel was entered, and the panel
repaints on each record only until `params::init`, or throughout on a boot
naming `early-panel`. A fault's vector, `RIP` and `CR2` reach no channel.

Exit: an exception table the kernel owns from its first statement, whose every
vector reaches the panic path — so a fault anywhere in the window is a panic
report on the panel and in the black box.
