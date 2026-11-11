---
status: open
kind: finding
opened: 2026-11-11
---

# A pipe gives back the frames its reader empties and takes them again

A pipe's bytes live in frames it takes as writes need them and gives back as
reads empty them (`kernel/src/pipe.rs`, `kernel/src/frame_bytes.rs`). Under a
steady stream the same frames leave the pipe and come back: a small write that
finds no room in a frame the pipe holds cannot be put in one hold of the
pipe's lock under its process's (`PipeRef::put_now`), and takes the general
path, which lets the lock go to take frames.

On the T14, ToyOS at `422915e2c` with stage timers in a scratch kernel,
`pipe.throughput.4096` with osbench's own buffers, two passes of 100 ms
([evidence](https://github.com/ToyOSOrg/evidence/tree/cb8da5fc2168bfe0395b6bce364212b4117a170d/diagnosis)):
- 35,552 of 102,720 and 38,050 of 105,216 writes (35% and 36%) took the
  general path, at 609 and 616 ns each, against 447 and 475 ns for a write put
  in one hold;
- a read on the general path spent 412 and 418 ns after its copy, consuming
  the frames it emptied, giving them back and waking the writer.

Exit: the cost measured against a pipe that keeps the frames its reader
empties for its next writes, on both benches, and that change made or the
cost recorded at the pipe's module.
