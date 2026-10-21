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

Only the pipe can keep such a write whole: its writers are separate
processes holding one pipe and sharing no lock, so no userland library can
order them.

The change is one branch in the pipe's write path, plus libc's constant, and
it costs two semantics. A write of at most `PIPE_BUF` waits for room for the
whole write (`pipe::has_space(id, len)`) where today it takes part of the
ring, and a non-blocking one (`sys_write_nonblock`,
`kernel/src/syscall/io.rs`) answers `WouldBlock` where today it answers a
short count, as POSIX has `O_NONBLOCK` do. Writes longer than `PIPE_BUF`
behave as today.

Owner: the kernel's pipe. It is the owner's call because it changes the
kernel's write semantics (ruling M: no kernel change for POSIX, so the case
for it is the native one above).

## Exit

A pipe write of at most `PIPE_BUF` bytes lands whole or waits for room, a
non-blocking one lands whole or answers `WouldBlock`, libc declares
`PIPE_BUF`, and a host test of the pipe shows two writers' writes of at most
`PIPE_BUF` never interleave while a reader lags. Longer writes may still be
split.
