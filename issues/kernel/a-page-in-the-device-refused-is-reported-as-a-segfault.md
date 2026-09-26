---
status: open
kind: defect
opened: 2026-09-26
---

# A page-in the device refused is reported as a segfault

A demand fault on a file-backed region whose backing's `read_page` fails logs
one line and leaves the fault unhandled (`kernel/src/process.rs`, the
`FileBacked` arm of the demand-page fill: "`<addr>` is backed by a file byte
`<n>` that the device would not read; leaving the fault unhandled"). The
exception path then reports the program's own fault
(`kernel/src/arch/idt/exceptions.rs`, the `theirs` arm): `SEGFAULT tid=…:
execute unmapped address at …` for a text page, `read unmapped address` for
data. The process dies as a program bug would, and its exit is the one a
wild pointer gives, so a supervisor, a test or a user reading the crash
report is told the program is wrong when the disk is.

The one line naming the device precedes the report, from a different
subsystem, and nothing ties the two together.

## Where it is reachable

Not for ROOT: `/system` is served from the image the loader put in memory,
and `ReadOnlyBacking::read_page` (`kernel/src/file_backing.rs`) fails only on
a block outside that image, which is a corrupt image rather than a device.
Every other file a process maps or runs is backed by a device: `NvmeBacking`
on DATA and the FAT backing on `/boot` and `/log`, so a program or library run
from `/home` or `/boot`, and any file mapped from them, reaches it.

## Owner

The demand-fault path in `kernel/src/process.rs` and the exception report in
`kernel/src/arch/idt/exceptions.rs`.

## Exit condition

A fault left unhandled because the backing refused the read ends the process
with a report and an exit that name the device failure and the file, not a
protection or mapping fault, and a guest test that fails a DATA-backed
binary's page-in (the `blkdebug` pattern `root_chunk_refused` uses) asserts
that report and that it contains no `SEGFAULT` line.
