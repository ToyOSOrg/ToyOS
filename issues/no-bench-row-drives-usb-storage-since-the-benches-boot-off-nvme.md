---
status: open
kind: tooling
opened: 2026-10-28
---

# No bench row drives USB storage since the benches boot off their NVMe

`toyos-metal` writes each image to the bench's internal install area
(partitions 5–11 after the bench's four, `tests/metal/src/internal.rs`) and
boots it once through `BootNext`, so no stick is in a bench boot's path. The
supervisor's `@boot` is then the NVMe controller, a test image's usbd row
drives no controller on a bench, and every metal judgement whose subject was
the boot stick lost it:

- `usbd_serves_the_boot_stick` is deleted: a bench's boot controller is no
  xHCI.
- `stick_survives_a_reset_under_load` is `the_disk_survives_a_reset_under_load`:
  the deadline reset under a write to the bench's NVMe. Whether a reset
  leaves a USB stick its next host can enumerate, `boot.usbload.stick_secs`
  and the guard `a-cut-bulk-only-command-does-not-brick-the-benchs-stick.md`
  names, is judged by no row.
- `tone_beside_a_stick_write` and `mask_windows` under `stick_write` write
  `/log`, which is the NVMe now: the USB load
  `a-megabyte-written-to-the-stick-starves-a-tone-beside-it.md` asks about is
  not reproduced on a bench. Their jobs and the records'
  `boot.windowscase.stick_write.*` still say "stick"; those two rows of each
  bench's record were taken under the USB load and are re-recorded once
  deleted.
- `bench_data_designated` no longer reads the T14 stick's short last sector.
- `amdvi_translates_the_boot_controller` and `amdvi_remaps_the_boot_controller`
  judge the NVMe controller where they judged the stick's xHCI.

QEMU's machines still boot off a stick (`the_log_lands_on_the_boot_stick`).
The test images' `pci:0c:03:30@boot` entry names QEMU's NEC controller, which
the entry before it already names, and nothing on a bench: whether `@boot`
keeps a user is the owner's to say.

**Exit**: a metal row drives a USB stick that is not the boot disk — the
scratch disk (`issues/the-scratch-disk-adopt-scratch-makes-is-not-served.md`)
— and judges usbd serving it, a reset under a write to it, and a tone beside
that write.

**Owner**: the orchestrator, which runs metal.
