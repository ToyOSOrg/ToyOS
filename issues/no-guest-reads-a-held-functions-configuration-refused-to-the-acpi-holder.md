---
status: open
kind: defect
opened: 2026-11-02
---

# No guest reads a held function's configuration refused to the `acpi` holder

The `acpi` claim's holder may write a function's configuration past its
header unless the function is held: a kernel driver drives it or a claim holds
it (`toyos_userbound::firmware::ConfigAt::write`, host-tested in
`lib/userbound/tests/firmware.rs`). What the kernel feeds that decision,
`pcidev::enumerated`'s `held`, was read on a guest by `acpi_mediated`
(`tests/guest/rust-tests/src/bin/acpi_mediated.rs`) on the console's
virtio-serial function, refused `HeldFunction` by address and through ECAM.

That arm was removed at #934's merge of `main` `058bea2d3`. Main moved the
console to the userland virtio-serial driver's claim (#1062), and since #934
every PCI claim waits for the root bridges' declaration. `acpicase`'s `acpi`
holders are jobs that declare nothing, so on that boot no claim is held and
no function is kernel-driven on q35 with VT-d: the arm had no held function
to ask about. The boot now runs on `Profile::HeadlessNoConsolePort`.

**Owner**: the ACPI track
(`issues/toyos-runs-the-machine-in-acpi-mode-and-interprets-its-aml.md`),
with `issues/the-acpi-servers-death-has-no-readback-on-the-t14.md`, which
has the same cause.

**Exit**: a guest boot where the `acpi` holder is a probe and a function is
held, by a claim made after a declaration or by a kernel driver, reads that
function's writes refused `HeldFunction` by address and through ECAM.
