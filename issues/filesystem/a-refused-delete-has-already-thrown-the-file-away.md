---
status: open
kind: tooling
opened: 2026-09-05
---

# No test stages a delete the device refuses

A delete that fails on the device has to leave the file as it was: its name,
its blocks, and every holder's view of it. `userland/fsd` asks the volume
first and only then marks the open file gone (`DataVolume::unlink` in
`userland/fsd/src/data.rs`, `FatVolume::unlink` in `userland/fsd/src/fat.rs`),
which is the order that keeps it; the kernel adapter this file was filed
against discarded the cached file, the name and the blocks before it asked.
Nothing stages the refusal, so the order is read off the code and not run.

**Exit**: a host test in `userland/fsd` whose disk refuses the reads or writes a
delete makes, and finds the name, the bytes and an open holder's reads as they
were before the refused call.
