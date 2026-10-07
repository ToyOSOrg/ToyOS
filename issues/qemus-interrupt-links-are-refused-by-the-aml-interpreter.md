---
status: open
kind: defect
opened: 2026-10-07
---

# QEMU's interrupt links are refused by the AML interpreter

`userland/acpiserver/aml/src/field.rs`'s `pci` finds the host bridge a
PCI_Config region is below as the nearest scope that names a `_BBN`. QEMU
11.1.1's DSDT (`toyos-acpi/fixtures/qemu-11.1.1/dsdt.bin`) names none: its
`\_SB.PCI0` is a host bridge by its `_HID` and `_CID` alone, on bus 0. So a
field of the region its ISA bridge declares is refused as `Unsupported`, and
with it every method that reads one.

Measured by evaluating each method that DSDT defines without an argument,
once, against a host that answers every read with zero: 39 methods, 15 a
value, 24 refused with `a PCI_Config region below no host bridge, which names
a _BBN` — the eight interrupt links' `_STA`, `_CRS` and `_DIS`. The T14's
host bridge names a `_BBN`, and none of its methods is refused this way.

Nothing uses the interrupt links yet; whoever routes a PCI interrupt through
the interpreter on QEMU does.

**Exit condition**: a host test in `userland/acpiserver/aml/tests/` evaluates
`\_SB.LNKA._CRS` on that fixture to a buffer, the host bridge found by what
the specification names one by (§6.1.5 `_HID`, §6.1.2 `_CID`).
