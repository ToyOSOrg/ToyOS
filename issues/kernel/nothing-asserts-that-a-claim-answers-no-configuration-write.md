---
status: open
kind: tooling
opened: 2026-09-08
---

# Nothing asserts that a claim answers no configuration write

`SYS_DEVICE_REG_WRITE` on a `RegTarget::PciConfig` target is refused
`NotSupported` in `kernel/src/arch/syscall/device.rs`, and no test in any tier
reads that refusal. It is what a handed-over MSI function's safety rests on: its
message address and data are words of configuration space rather than a table in
a BAR, so nothing is withheld from the holder and the whole of the boundary is
that the write path does not exist. A one-field mutation there — the arm
answering `Ok` — hands the holder the ability to aim the device's write at any
address the LAPIC decodes, and every arm in every tier stays green.

The SDK's `PciDev` offers `config_read` and no write, so a driver cannot express
the call without reaching past it into `toyos_abi::syscall`.

Owned by whoever next adds a boot config with a test binary holding a claimable
function. Exit condition: a guest arm in which the holder calls
`SYS_DEVICE_REG_WRITE` on its own claim and the kernel refuses it, red against a
kernel whose `PciConfig` write arm answers `Ok`.
