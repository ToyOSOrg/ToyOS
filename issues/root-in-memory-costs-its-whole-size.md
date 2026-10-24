---
status: open
kind: finding
opened: 2026-09-25
---

# ROOT in memory costs its whole filesystem, in RAM and in a read every boot

The loader reads ROOT's filesystem whole, the extent its superblock states
(`loader/src/rootimage.rs`), and the kernel keeps it for the machine's
life, read-only, for the supervisor's SYSTEM file server to serve
(`kernel/src/bootimage.rs`, `system/fileserver/src/root.rs`). `Superblock::check` holds that extent equal to the partition, and
`build/image.rs` builds the volume at the blocks its contents use, rounded up to
the 1 MiB partition alignment, so what is read and kept is what ROOT carries.
The cost is the payload.

**A test boot's ROOT carries the test binaries that boot runs and no others.**
`CARRIES` in `tests/toyos.rs` names what each machine and screen test runs, an
audio run carries its one binary, and the shared list runs on boots of one lane
in turn, each carrying at most `SHARED_BOOT_BYTES` (64 MiB) of test binaries;
`qemu::carrying` adds what those binaries spawn or load. A boot that runs none
carries `tests/images/testcases.toml`'s own programs, 13 MiB (3328 blocks), where every test
boot carried the whole 619 MiB catalogue before.

**A running `/system` program's pages are in RAM twice.** Its spawn names
SYSTEM's object for the file, a subrange of ROOT's own pages (`fs::IMAGE`,
`SYS_SHM_SUB`), and `SharedImage::read_page` (`kernel/src/file_backing.rs`)
copies each page the child touches out of it: ROOT's copy and the page's are
held. Until SYSTEM handed that object out, each spawn also read the whole
binary into an image object of the spawner's, a third copy, and the
measurements below are of that design. Measured on QEMU's x86-64 machine booting
`tests/images/testcases.toml`, toybox's `free` after the boot: 330 MiB in use
at `b9e4c5862`, 384 MiB once ROOT left the kernel, one boot each, the same
session; the 54 MiB are the running services' images, each rounded up to
2 MiB, and the SYSTEM file server.

**Per held child, the image was 2 MiB of a 16 MiB cost, and it was what put
255 children past a 4 GiB guest**, while each spawn read one; a `/system`
program's spawn now holds no image object of its own. `libc_children`'s `spawn_children_cap`
holds 255 unwaited children of one `/system` program. Measured on QEMU's x86-64
machine, `tests/images/libccase.toml`, 4 GiB, toybox's `free` every 50 held
children, one boot each in the same session (a scratch patch to the case, not
committed):

| held | used at `6ae79ccaa` (kernel pages from ROOT) | used at `25d787482` (spawn reads an image) |
|---|---|---|
| 0 | 126 MiB | 178 MiB |
| 50 | 836 MiB | 988 MiB |
| 100 | 1546 MiB | 1798 MiB |
| 150 | 2254 MiB | 2606 MiB |
| 200 | 2964 MiB | 3416 MiB |
| 250 | 3674 MiB | refused at 241 (`ENOMEM`) |

That is 14.2 MiB a child at the base and 16.2 MiB with the image, the
difference one image rounded up to 2 MiB. Each child's exit record says
`peak=2MB allocs=2` in both: `peak` counts what a process allocated, and neither
its image nor what the kernel holds for it. So the image costs the case its
last 14 children, and the 14.2 MiB beside it are 2 MiB pages, which
`process-memory-is-2-mib-pages-and-that-caps-the-process-count.md` removes.

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

Exit: a program's pages mapped from ROOT's own read-only pages rather than
copied into each process, before the self-update's signature check makes the whole read a fixed
cost. The orchestrator ruled the mechanism (2026-10-22): the memory design's
stages 2, 4 and 6, namely 4 KiB pages, so an image is a program's size and not
2 MiB; memory-object subranges; and immutable file objects shared across
spawns, so that SYSTEM's object for a ROOT file is a subrange of the boot
image. No dedicated call over part of a boot image is to be built.
`spawn_children_cap` holds all 255 children again once that has landed.
