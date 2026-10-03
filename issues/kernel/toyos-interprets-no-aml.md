---
status: open
kind: track
opened: 2026-10-03
---

# ToyOS interprets no AML

Nothing in the tree evaluates the AML in a machine's DSDT or SSDTs. What
ToyOS takes from them it takes another way: `toyos-acpi/src/dsdt.rs` finds
`\_S5_` by a byte scan, and the loader asks UEFI for the root bridges'
windows `_CRS` would name (`bootloader/src/rootbridge.rs`). Nothing reads
`_CST`, which names a CPU's C-states.

The owner's direction, recorded in `0ee814f5a`: ToyOS writes its own AML
interpreter, and the battery comes first.
