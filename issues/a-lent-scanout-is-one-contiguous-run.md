---
status: open
kind: defect
opened: 2026-10-14
---

# A lent scanout is one physically contiguous run

The compositor lends each virtio-gpu scanout as one region
(`system/compositor/src/display.rs`, `Claim::lend`), and a region a device
may be lent is one physically contiguous run of 2 MiB pages: the client asks
for it with `SHM_DEVICE`, and the kernel makes it with
`SharedMemObject::create_run` (`kernel/src/object/shm.rs`), while every other
region is scattered 4 KiB frames. A mode at the
largest bound is 64 such pages; on a machine whose memory is fragmented there
may be no run that long, and the mode change is then declined by name and the
old mode stays, though the memory exists.

The window a claim lends in is placed first-fit too (`kernel/src/pcidev/mod.rs`,
`Bound::lend_room`). `toyos-desktop` sizes `LEND_WINDOW` for two scanouts at
the largest mode, which holds only while nothing splits it: boot at 1280x800
(2 MiB pages 0-1), flip to 7680x4320 (pages 2-65, freeing 0-1), then ask for
7680x4300 (63 pages) — the free runs are 2 and 62 pages and the flip is
declined by name, the old mode kept. Only modes past about half the window
reach it.

**Lendability is the creator's choice, and its failure is the lending
peer's.** A region another process lends is made by its creator, who must
ask for `SHM_DEVICE` without being the one that lends it: soundserver makes
the ring the HDA driver lends (`system/soundserver/src/output.rs`), and a
filesystem client makes the region `toyos-blockserve` lends to its disk. A
creator that makes an ordinary region gets one; only the driver's lend
refuses it, as `Unlendable`, and then the device plays or reads nothing. On
the T14 every audio test failed that way until soundserver asked for
`SHM_DEVICE` (QEMU's `hda_plays_to_the_controller_that_reaches_an_output`
reds the same way). Lending scattered frames removes the choice, and the
failure with it.

Virtio 1.2 §5.7.6.1 already takes a
scatter list ("the framebuffer doesn't need to be contignous in guest
physical memory"), and `RESOURCE_ATTACH_BACKING` takes one entry per run.

Owner: the memory track. Exit: any region of RAM lendable however its
creator made it, backed by more than one run and lent as one mapping in the
function's domain or as one entry per run; a mode change on a machine with
no run of its length served; `SHM_DEVICE` and `create_run` deleted, since no
creator then chooses a physical layout for a peer; and a lend window whose
free addresses serve any lend no larger than their total.
