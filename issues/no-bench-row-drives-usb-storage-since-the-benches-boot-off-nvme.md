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
- `tone_beside_a_stick_write` and `mask_windows` under `stick_write` are
  `tone_beside_a_log_write` and `mask_windows` under `log_write`: they write
  `/log`, the NVMe now, so the USB load
  `a-megabyte-written-to-the-stick-starves-a-tone-beside-it.md` asks about is
  not reproduced on a bench. The windows' records are
  `boot.windowscase.log_write.*`, recorded afresh under the NVMe load; the
  `stick_write` rows of each bench's record were taken under the USB load.
- `bench_data_designated` no longer reads the T14 stick's short last sector.
- `amdvi_translates_the_boot_controller` and `amdvi_remaps_the_boot_controller`
  judge the NVMe controller where they judged the stick's xHCI.

QEMU's machines still boot off a stick (`the_log_lands_on_the_boot_stick`).

No row that writes the scratch disk is built. The owner declined the
comparison of ToyOS's resets with Linux's restart
(`issues/a-reset-can-leave-a-usb-stick-hung-until-its-power-is-removed.md`),
and that does not discharge this exit.

**A stick's forced writes have never met a real stick.** This is not a
judgement the boot stick lost: it is new with the forced write. usbd asks
MODE SENSE(6) as a disk first comes up, reads DPOFUA from its header, and
sends a durable write as WRITE(10) with FUA only where the stick set it
(`system/usbd/xhci/src/msc.rs`, `lib/scsi`). A host model and QEMU's stick
are all that have answered either command. What a real stick can do that
they do not:

- **Break on the MODE SENSE.** The command climbs the recovery ladder like
  any broken command, and since it runs at every first bring-up, a stick
  that hangs on it is never served.
- **Set DPOFUA and refuse a write with FUA.** The write is answered not
  done; the journal issues its entry again twice more and then the mount stops
  writing. Nothing acknowledged is lost, and the volume takes no more
  writes until it is mounted again.
- **Set DPOFUA and ignore FUA.** The write is answered done from the
  stick's cache. The commit is acknowledged without being on the medium, a
  power cut loses it, and usbd counts no flush owed for it, so a stick that
  leaves then is counted as having lost nothing. Nothing on the host can
  see this.

**Exit**: a metal row drives a USB stick that is not the boot disk — the
scratch disk (`issues/the-scratch-disk-adopt-scratch-makes-is-not-served.md`)
— and judges usbd serving it, a reset under a write to it, and a tone beside
that write. And for forced writes: on a real stick, usbd's bring-up line
shows the MODE SENSE answered with no Bulk-Only break before it; and on a
real device that sets DPOFUA, carrying a bcachefs volume, a commit's
WRITE(10) with FUA is answered done. Whether such a device honours FUA is
shown only by cutting its power after an acknowledged commit; that is
evidence to record if it is ever taken, and no part of this exit.

**Owner**: the orchestrator, which runs metal.
