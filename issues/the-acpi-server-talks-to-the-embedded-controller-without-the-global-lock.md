---
status: open
kind: defect
opened: 2026-10-04
---

# The ACPI server talks to the embedded controller without the global lock

A machine's AML names whether its firmware also reaches the embedded
controller from SMM: the controller device's `_GLK` returns 1, and then every
transaction is taken under the FACS global lock (ACPI 6.5 §6.5.7, §5.2.10.1).
`/system/bin/acpiserver` evaluates `_GLK` only to read no battery where it is
1 (`userland/acpiserver/src/battery.rs`), and takes no lock around its own
queries: on such a machine a query of the server's and one of the
firmware's can interleave on the same two ports. The lock itself is there to
take: the interpreter's host takes it for a Lock field or an Acquire of
`\_GL`, waiting out the firmware's hold on its `GBL_STS`
(`userland/acpiserver/src/host.rs`); nothing takes it around a controller
transaction.

On the T14 no table names a `_GLK` at all: a byte search of its DSDT and every
SSDT for the name finds none (the tables captured before its wipe, read
outside the tree). Every other machine is unread.

Owned by `issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`,
whose interpreter is what can evaluate it.

**Exit**: the server takes the global lock around each controller
transaction wherever the controller's `_GLK` evaluates to 1, and a test drives
a controller that requires it.
