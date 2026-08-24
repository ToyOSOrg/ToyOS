---
status: open
kind: defect
opened: 2026-08-01
---

# A machine that boots off its internal disk gets no `/boot`

`gpt::probe` runs twice now — `kernel_main` asks the NVMe namespace, and
`fat32_adapter::probe_boot_disks` asks every USB disk — so the stick this
project boots from is found and `/boot` mounts. The NVMe call is the one that
cannot lead anywhere: `page_cache::init` takes sole ownership of the device
immediately afterwards, so even when the boot partition *is* on the internal
disk, `gpt::boot_volume()` names a device nothing can hand the FAT32 adapter.
That is the installed-ToyOS case, which is where this ends up.

The `Resolution::Ambiguous` arm is now live and exercised: `boot_partition_identity`
puts the image's own partition GUID on a crafted NVMe disk while the real stick
carries it too, and the machine correctly reports it has no boot volume. Worth
knowing before adding a third probe — two devices claiming one unique partition
GUID poisons the answer permanently, by design.

**2026-08-25, promoted to `defect`.** Re-verified: `kernel/src/main.rs` calls
`gpt::probe(&mut nvme_dev, sector_size)` at line 547 and
`page_cache::init(Box::new(nvme_dev))` at line 548, so the NVMe probe's answer
names a device that has already been moved out and nothing can hand it to
`fat32_adapter`. A machine booting off its own disk getting no `/boot` is the
installed product's ordinary case, not a dev-stick curiosity, and
`issues/build/the-initrd-is-still-the-root-filesystem.md` is already blocked
behind it. Owed by whoever builds installation.
