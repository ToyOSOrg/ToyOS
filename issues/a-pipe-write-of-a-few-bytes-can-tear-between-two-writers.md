---
status: owner
kind: question
opened: 2026-10-21
---

# A pipe write of a few bytes can tear between two writers

`pipe::try_write` (`kernel/src/pipe.rs`) writes `min(len, free space)` and
`sys_write` (`kernel/src/syscall/io.rs`) returns that short count. It waits
only while the ring is full, and the caller's write loop sends the rest later.
So whenever the reader lags and the ring has less free space than a write,
even a 40-byte line is split. Another writer's bytes can land between the two
parts. POSIX promises a pipe write of at most `PIPE_BUF` bytes is never
interleaved with another's, and `sdk/libc/include/limits.h` declares no
`PIPE_BUF` at all.

Every multi-writer pipe has this gap: `{ a & b; } | c`, `make -j | tee log`,
and a stdout stream several children share, whose lines tear in one log
while the file server behind it is busy. A one-byte write, such as a
jobserver token, cannot tear. Record boundaries matter to any
multi-writer pipe, so the fix is native, not POSIX's alone.

Owner: the kernel's pipe. It is the owner's call because it changes the
kernel's write semantics (ruling M: no kernel change for POSIX, so the case
for it is the native one above).

## Exit

A pipe write of at most `PIPE_BUF` bytes lands whole or waits for room
(`pipe::write_whole` already does exactly that for messages), libc declares
`PIPE_BUF`, and a host test of the pipe shows two writers' writes of at most
`PIPE_BUF` never interleave while a reader lags. Longer writes may still be
split.
