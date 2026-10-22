---
status: open
kind: defect
opened: 2026-09-29
---

# A user-copy pin that is never released reds no test

`kernel/src/user_ptr.rs`'s copy loop pins each frame of a chunk
(`pmm::pin_range`) and unpins it after the copy (`pmm::unpin_range`). The pin
counters and the deferral of a frame freed under a pin are host-tested
(`kernel::frames`), but the loop's own unpin is reached by no test: with it
removed, every frame a syscall ever copied through keeps a pin, and a frame
freed later is deferred for the rest of the boot. `pmm::stats` then counts it
used, but nothing reads that, so the machine loses memory no test sees.

## Exit condition

A test that goes red under that mutation: for example, a guest job that copies
into and unmaps more frames than the guest has, or a count of frames still
holding a pin and a test that asserts it returns to zero once no copy is in
flight. Then this file is deleted.

## Owner

`kernel/src/user_ptr.rs`, `kernel/src/mm/pmm.rs`. Nobody holds it.
