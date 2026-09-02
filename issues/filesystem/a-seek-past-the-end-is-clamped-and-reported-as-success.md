---
status: open
kind: defect
opened: 2026-09-02
---

# A seek past the end is clamped to the size and the clamped offset is returned as success

`kernel/src/object/ops.rs:451` ends `SYS_SEEK` with

    state.position = (new_pos as usize).min(size);
    state.position as u64

so `lseek(fd, size + n, SEEK_SET)` moves to `size`, and the return value is
`size` rather than the offset that was asked for. POSIX `lseek` requires the
opposite: seeking past the end is legal, the resulting offset is what the caller
named, and a write there extends the file with a hole that reads as zeros in
between.

Two things are wrong and only one of them is loud. A caller that reads its own
return value sees a smaller number than it asked for and can notice. A caller
that does not — `lseek` then `write`, which is how every sparse file in POSIX is
made — silently writes at `size` instead of at `size + n`, so the bytes land in
the wrong place with no error anywhere.

`userland/libc/src/posix_io.rs`'s `lseek` passes the kernel's answer straight
back, so the clamp is the whole behaviour a C program sees.

Not this branch's: the clamp predates it and no commit here touches `ops::seek`.
It is filed rather than fixed because the write path behind it is the same
hole-filling machinery two of this branch's commits are already changing, and a
seek that can name an offset past the end needs that machinery to be right
first.

**Exit condition.** `lseek` past the end returns the offset it was given; a
write there extends the file; and the bytes between the old end and the write
read as zeros on every mount — asserted from the device, not only the cache,
because that is where the two mounts have disagreed before.
