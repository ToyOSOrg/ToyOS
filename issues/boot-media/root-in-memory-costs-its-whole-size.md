---
status: open
kind: finding
opened: 2026-09-25
---

# ROOT in memory costs its whole filesystem, in RAM and in a read every boot

The loader reads ROOT's filesystem whole, the extent its superblock states
(`bootloader/src/rootimage.rs`), and the kernel keeps it for the machine's
life. `Superblock::check` holds that extent equal to the partition, and
`src/image.rs` builds the volume at the blocks its contents use, rounded up to
the 1 MiB partition alignment, so what is read and kept is what ROOT carries.
The cost is the payload. The test suite's ROOT carries every C and Rust test
binary the shared boot runs: 619 MiB (158464 blocks). Of that, 132 MiB is
`demand_window_race`'s `.bss`, zeros `toyos-ld` writes into the file
(`issues/hardware/anonymous-mmap-is-not-demand-paged.md`), which a guest that
mounts ROOT off a disk never reads and a guest here holds.

**Every resident `/system` page is in RAM twice.** `ReadOnlyBacking::read_page`
(`kernel/src/file_backing.rs`) copies each page a program touches out of the
image into a file-cache page, so the image's copy and the cache's are both
held.

**On the dev host the fast tier's peak host memory rises with it.** An
alternating A/B of `cargo test` against `origin/main` at `b0adc600`, three arms
each in one session, summing the RSS of the run's QEMU processes every 2 s:
`main` peaked at 5801, 6367 and 6275 MiB, and the branch, with a 680 MiB ROOT
that still carried a tenth of free blocks, at 7867, 9634 and 10836 MiB, and a
fourth branch run with the trimmed 619 MiB ROOT at 10591 MiB. No arm of either
used swap, and the kernel's `memorystatus_level` never fell below 73.

The read, QEMU 11 TCG, host clock and TSC agreeing, measured before the free
blocks were trimmed: the `--build-only` image's 117 MiB ROOT in 98–111 ms off
`usb-storage` and 20 ms off `virtio-blk`; the test image's 669 MiB in 547–580 ms
off `usb-storage`. Against the base, that image's time to `Boot: complete` on
USB moved from about 1.19 s to 1.29 s of host wall clock, the kernel's own boot
unchanged (219–229 ms both).

Not measured: the T14. What that run owes is the read's TSC cycles off the
stick and off NVMe (`ROOT: read into memory at … in N TSC cycles` in
`loader.log`, converted by the kernel's `TSC:` record), and the same image's
time to `Boot: complete` against the base.

Exit: either measured and accepted by the owner as the price, or ROOT's image
cut to what a boot needs (the test image's payload split off it, as a ROOT per
test profile or the test binaries on DATA, and `.bss` left out of the file by
`toyos-ld`) and the file cache serving the image's own page rather than a copy,
before the self-update's signature check makes the whole read a fixed cost.
