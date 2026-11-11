---
status: open
kind: tooling
opened: 2026-09-05
---

# No test stages a delete the device refuses

A delete that fails on the device has to leave the file as it was: its name,
its blocks, and every holder's view of it. The file server asks the volume
first and only then lets the open file go: DATA's `unlink`
(`system/fileserver/src/data.rs`) releases the file's blocks only after the
engine's unlink answered, and FAT's (`system/fileserver/src/fat.rs`) marks the
open file gone only after `remove` did. Nothing stages the refusal, so the
order is read off the code and not run. FAT's
`an_unlink_stopped_anywhere_is_recovered` stops the device for good and judges
the next mount, not a server that goes on answering.

**Exit**: a host test in `system/fileserver` whose disk refuses the reads or
writes a delete makes, and finds the name, the bytes and an open holder's
reads as they were before the refused call.
