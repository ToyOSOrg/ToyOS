---
status: open
kind: defect
opened: 2026-10-11
---

# A pipe, console, socket or shared memory has no identity

`ops::fstat` (`kernel/src/object/ops.rs`) answers `volume: 0, node: 0` for
every object but a file: a pipe or tty end, a connection, an acceptor, a
console, shared memory, a device and the capability objects. libc answers the
pair as `st_dev` and `st_ino` and std as `MetadataExt::dev` and `ino`, so
every such descriptor is one file to a program that asks: two pipes, a pipe's
two ends and a console beside a pipe all compare equal, and `0` is the pair a
caller reads as "nothing". POSIX's `<sys/stat.h>` says `st_ino` and `st_dev`
uniquely identify a file, a pipe included.

**Read off the code; no program that depends on it has been seen to fail.**

**Exit condition.** `fstat` of two pipes answers two nonzero pairs, both ends
of one pipe answer one, and a console answers one apart from them; a guest C
case reads them.

**Owner.** `kernel/src/object/ops.rs`. Nobody holds it.
