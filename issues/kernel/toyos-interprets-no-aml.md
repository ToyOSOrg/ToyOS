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

Two tracks wait on it: `issues/kernel/the-scheduler-and-the-clocks-never-talk.md`
and `issues/kernel/idle-cpus-enter-the-c-states-cst-names.md`.
