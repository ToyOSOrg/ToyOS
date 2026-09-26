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
The cost is the payload.

**A test boot's ROOT carries the test binaries that boot runs and no others.**
`CARRIES` in `tests/toyos.rs` names what each machine and screen test runs, an
audio run carries its one binary, and the shared list runs on boots of one lane
in turn, each carrying at most `SHARED_BOOT_BYTES` (64 MiB) of test binaries;
`qemu::carrying` adds what those binaries spawn or load. A boot that runs none
carries `tests/testcases`'s own programs, 13 MiB (3328 blocks), where every test
boot carried the whole 619 MiB catalogue before. The one part past the bound is
`demand_window_race`, 131 MiB, nearly all of it the `.bss` zeros `toyos-ld`
writes into the file (`issues/hardware/anonymous-mmap-is-not-demand-paged.md`),
which a guest that mounts ROOT off a disk never reads and a guest here holds.

**Every resident `/system` page is in RAM twice.** `ReadOnlyBacking::read_page`
(`kernel/src/file_backing.rs`) copies each page a program touches out of the
image into a file-cache page, so the image's copy and the cache's are both
held.

**On the dev host the fast tier's peak host memory is now below `main`'s.** An
alternating A/B of `cargo test` against `origin/main` at `b0adc600`, three arms
each in one session, summing the RSS of the run's QEMU processes every 2 s:
`main` peaked at 6238, 5662 and 6486 MiB and the branch at 5149, 4546 and
5202 MiB, no arm using swap. The test process itself peaked at 13404, 15340 and
15284 MiB on `main`, which memoises every whole-catalogue ROOT it builds, and at
4621, 5100 and 4689 MiB on the branch. With one shared boot carrying all
416 MiB of the shared binaries, that guest was the run's largest, at 3.0 GiB of
RSS against `main`'s largest at 2.6 GiB.

The read, QEMU 11 TCG, host clock and TSC agreeing, measured before the free
blocks were trimmed: the `--build-only` image's 117 MiB ROOT in 98–111 ms off
`usb-storage` and 20 ms off `virtio-blk`; the test image's 669 MiB in 547–580 ms
off `usb-storage`. Every boot now says its whole cost on one kernel line,
`boot: power-on to loader N ms, loader M ms (ROOT read R ms), kernel to Boot:
complete K ms`; a test boot on QEMU reads 1390, 108 (14) and 344.

Not measured: the T14. What that run owes is that line off the stick and off
NVMe, against the base's `Boot: complete`.

Exit: either measured and accepted by the owner as the price, or `.bss` left
out of the file by `toyos-ld` and the file cache serving the image's own page
rather than a copy, before the self-update's signature check makes the whole
read a fixed cost.
