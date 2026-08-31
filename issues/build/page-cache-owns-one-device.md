---
status: open
kind: defect
opened: 2026-08-07
---

# The page cache owns one device, and `usb_storage.rs` says it does not

`page_cache.rs:11-12` holds exactly one
`BLOCK_DEV: Lock<Option<Box<dyn BlockDevice>>>`, `page_cache::init` takes
ownership of the NVMe device, and `PageCache::_device_id` is written at
construction and read nowhere. So `usb_storage.rs:14-17`'s comment — *"NVMe
takes 1; the page cache keys itself on this, so two devices sharing a number
would serve each other's blocks"* — describes a mechanism that does not exist.
The numbers are right and the keying is not.

`fat32_adapter.rs:911-915` states the live consequence and does not work around
it: a machine that boots off an **internal** disk gets neither `/boot` nor
`/log`, "because the NVMe device is owned by the page cache from the moment
storage comes up and there is no second handle to it". `/boot` and `/log` work
on the T14 and in QEMU only because the boot medium is USB, and
`usb_storage::open` mints a fresh handle per call.

**The mount side has one arm and no second one to reach for.**
`fat32_adapter::mount` resolves the `DeviceId` in `gpt::Volume` through
`usb_storage::open` (`kernel/src/fat32_adapter.rs:864`, and `:881` for the boot
probe's own walk), and there is nothing else it can call. Two fixes are named and
neither is a two-line change: a shared block-device handle, or moving the page
cache off sole ownership. Still reproducing when it was last checked at the site
on 2026-08-25.

**And it is the installed product's ordinary case, not a dev-stick curiosity.**
`gpt::probe` runs twice — `kernel_main` asks the NVMe namespace and
`fat32_adapter::probe_boot_disks` asks every USB disk — so the stick this project
boots from is found and `/boot` mounts. The NVMe call is the one that cannot lead
anywhere: `kernel/src/main.rs:367` probes `nvme_dev` and `:368` moves it into
`page_cache::init`, so `gpt::boot_volume()` names a device that has already been
moved out and nothing can hand it to the adapter. Owed by whoever builds
installation.

**Worth knowing before anybody adds a third probe.** The `Resolution::Ambiguous`
arm is live and exercised: `boot_partition_identity` puts the image's own
partition GUID on a crafted NVMe disk while the real stick still carries it, and
the machine correctly reports it has no boot volume. Two devices claiming one
unique partition GUID poison the answer permanently, by design.

This is the real cost of the root partition that never landed
(`issues/build/the-initrd-is-still-the-root-filesystem.md`): a bcachefs root on
the boot medium needs a `BlockIO` over an arbitrary `BlockDevice` at a partition
offset with a cache of its own, where `PageCacheBlockIO` *is* the NVMe device by
construction. Found 2026-08-07 while pricing that work — it was one of eight
items a USB storage driver was expected to bring, and the one that did not
arrive with it.
