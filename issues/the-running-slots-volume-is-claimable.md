---
status: open
kind: defect
opened: 2026-09-26
---

# The running slot's FAT volume is claimable, where its ROOT is not

The kernel holds the partition its ROOT image came from for the machine's life
(`kernel/src/rootfs.rs`, `hold_source`), so no claim can write the ROOT a boot
runs. It holds nothing of the same slot's FAT volume — the kernel, its boot
parameter and the signed header the loader read — so any process whose
`SysCap` carries `device` can claim that partition by its unique GUID and write
it. The test estate's `test-runner` holds `device`, and so would any row
a config gives it to.

`/system/bin/update` reaches it too. `slots::grant` (`toyos-update/src/slots.rs`)
knows which volume is the running slot's only from the slot table, and
`update` writes that table: one naming a decoy volume beside the running ROOT
as slot A, and the running volume beside the idle ROOT as slot B, is granted
the running volume on the next boot. Nothing the kernel holds names the booted
volume, so init has nothing to check the table's word against.

A write there is not a silent compromise: the loader holds every byte to the
signature at the next boot and refuses the slot by name. It is a denial —
the running slot is refused next boot and the machine falls back to an older
image, or to nothing on a one-slot image.

**Exit**: the loader hands the kernel the booted slot's volume by its unique
GUID beside ROOT's, and the kernel holds it as it holds ROOT's, so a claim on
it is refused as `PermissionDenied` like every partition the kernel holds; a
guest test asserts the refusal.
