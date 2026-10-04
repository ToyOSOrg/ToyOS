---
status: open
kind: defect
opened: 2026-10-04
---

# A held launcher that will not open is a direct spawn

std's ToyOS `launch` (`library/std/src/sys/process/toyos.rs` in the `rust/`
fork) reads `toyos::endow::launcher().and_then(|held| held.open(LAUNCHER).ok())`:
a process that holds a launcher and whose open of it fails is treated as one
that holds none, and its spawn of a declared program goes on as a direct spawn,
without that program's row and with no error. The shape is the one `main` had
before launch authority, where `endow::service("launcher")`'s `Err` did the
same.

Holding none and holding one that refuses are different facts: the first is a
direct spawn by design, the second is a failure the caller is never told of,
and the program it starts runs without what its row holds.

**Exit**: a spawn by a process holding a launcher whose open fails returns
that failure, and a test that makes the open fail reds when the spawn
starts the program.
