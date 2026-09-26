---
status: open
kind: defect
opened: 2026-09-26
---

# The anti-rollback floor is a firmware variable, which the machine in hand can reset

The loader refuses an image whose version is below the highest version a boot
has proven, and keeps that floor in `ToyOSImageFloor`, a UEFI variable it
creates non-volatile and boot-services-only (`bootloader/src/floor.rs`). That
holds against everything after the handoff: no kernel, no program and no write
to the disk can lower it, because the variable is unreachable once
`ExitBootServices` has run. It does not hold against:

- **anything firmware boots instead of the loader** — an EFI shell or another
  OS's loader off a stick deletes the variable with one `SetVariable`, and
  nothing stops firmware booting one until Secure Boot is on with the owner's
  key (the track's verified-boot stage);
- **the firmware's own reset of its variables** — a setup-menu "restore
  defaults", a CMOS clear, a flash of the firmware;
- **a floor that never rises** — it rises only on a boot that hands the machine
  back on purpose, so a machine that is only ever powered off keeps the floor of
  its last clean reboot, and the old slot beside it stays bootable.

`/system/bin/update`'s own check — newer than the running image — reads the
versions in the slot table, which is on the disk and is advisory; the loader's
floor is the enforcement.

**Exit**: the floor is a monotonic counter the platform refuses to lower — a
TPM 2.0 NV counter, extended by the loader and read by it — or the machine is
documented as having none and the loader says so on every boot; and the floor
rises on a boot that confirms itself healthy, not only on a reboot.
