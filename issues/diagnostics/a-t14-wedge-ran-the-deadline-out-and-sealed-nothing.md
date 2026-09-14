---
status: open
kind: defect
opened: 2026-09-14
---

# A T14 wedge ran the boot deadline out and left no WEDGED record, so the one instrument built for it reported nothing

T14 run 48, `tests/metalcase` on `wt/toyos-quiesce` `3f8f7fd3`, readback in
`target/metal-quiesce-run47/metalcase/`. The boot wedged and the stick came back
readable — the case
`issues/hardware/a-t14-boot-wedges-after-a-jobs-exit-and-nothing-said-why.md`
has been waiting for — and **the deadline sealed nothing**, so its exit
condition is still unmet.

## What the stick carries

The files are not the ones the driver extracted: it refused before extracting,
so these were read out of `log-partition.img` with `toyos-fat32` itself.

| | |
|---|---|
| kernel log | 312 lines, ending `[3.672 cpu5] shm: 0x4000000000 mapped WriteCombining into pid 5` |
| the next record, `spawn: /system/bin/sshd … total=1313ms` at 4.106 s | on the device, in a cluster **no directory entry reaches** |
| `loader.log` pass 2 | `Boot attempts: the previous boot of this image never reported; the machine is handed back` |
| a `Previous boot's panic:` line | absent |
| `WEDGED` in the partition | twice, both the boot-time announcements of the deadline and the lockup detector |
| ssh returned after | 190 s, against 50 s for `metaldevicecase` in the same run |

`deadlinewedge` (223 s) and `hardlockup` (171 s) in that same run both sealed
correctly and printed `Previous boot's panic: the last boot read WEDGED`, so the
seal path works on this kernel. Only the unplanned wedge produced none.

## Two things are wrong, and they are separable

**The deadline did not report.** The boot was armed with
`boot-deadline=120000` (its own `loader.log` line 9), it made no progress after
3.7 s, and it took ~140 s longer than a healthy boot of the same config — yet
the black box read neither `DONE` nor `WEDGED`, which is the loader's
"never reported" path. Either the deadline never fired, or it fired and its seal
did not reach the page. Nothing on the stick separates those.

**The reset tore the volume**, because nothing synced it: `toyos-fat32-check`
refused the partition with `1 cluster(s) from 71 are marked allocated and no
directory entry reaches them` and `FSI_Free_Count is 68484 and the FAT has 68483
free clusters`. That is the orphan cluster above — `logd`'s data reached the
device and its directory entry and FSInfo did not. It is what any reset without
`quiesce` leaves, and it is why the driver's verdict was EXIT=1 on a boot whose
actual failure was the wedge.

## Why it is filed here and not against the stop

`quiesce` never ran on this boot: the partition carries no `Syncing
filesystems...`, no `stop:` and no `Rebooting.`. So the torn volume is
downstream of the wedge, not of the shutdown's stop.

**Exit condition**: a T14 boot that wedges with the deadline armed and leaves a
`WEDGED` record naming what the machine was doing — or, where the deadline
genuinely did not fire, a reading that says so, since a bound that is armed and
silent is indistinguishable on the stick from one that never expired.
