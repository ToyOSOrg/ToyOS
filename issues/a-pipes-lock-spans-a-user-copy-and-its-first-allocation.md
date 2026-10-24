---
status: open
kind: defect
opened: 2026-08-28
---

# `pipe::try_read`/`write` hold the one global `PIPES` spinlock across a 2 MiB user copy, and a pipe's first write adds a bitmap scan and a 2 MiB zeroing under it

Every pipe and every IPC `Connection` in the machine lives behind one static,
`PIPES: Lock<Option<IdMap<PipeId, Pipe>>>` (`kernel/src/pipe.rs`).
`with_pipes_mut` and `with_pipes` run their whole closure under it, and
`sync::Lock::lock` disables preemption before it spins (`kernel/src/sync.rs`),
so the holder is non-preemptible and every other CPU that wants a pipe spins
with preemption off too.

## The mechanism

The bulk copy is inside that closure, not outside it.

* `pipe::try_read_within`, which `try_read` calls, runs
  `ring.read(len, |off, src| buf.write_run(off, &src))` inside
  `with_pipes_mut`.
* `pipe::write` runs `backing.ring.write(n, |off, dst| buf.read_run(off, dst))`
  in the same acquisition.
* `Ring::read`/`Ring::write` (`sdk/abi/src/ring.rs`) hand the closure one or two
  contiguous runs, and the closure copies each whole.

The size is bounded only by the ring: `PIPE_SIZE` is `PAGE_2M`, and the
capacity is that less the 64-byte `RingHeader`, so **2,097,088 bytes** is the
largest single copy under the lock. Nothing above caps it: `SYS_READ` and
`SYS_WRITE` pass the caller's length through (`kernel/src/syscall/dispatch.rs`),
and `object::ops::try_read` hands the whole buffer to `pipe::try_read`, which
copies it under the lock. Each 16 pages of that copy take the caller's space
lock and populate any page not yet touched — a frame from the allocator and a
4 KiB zeroing — inside `PIPES`, where the window used to be filled before the
lock was taken.

A pipe's **first** write is worse, because the page is allocated lazily under the
same lock. `write` calls `Pipe::back`, which calls `pmm::alloc_page`: the frame
allocator's lock nested inside `PIPES` for a free superframe, then a 2 MiB
zeroing (`kernel/src/mm/pmm.rs`), before `Ring::new` and the user
copy that follows it.

## What queues behind it

Everything pipe-shaped in the kernel goes through the same static: `create`,
`has_data` and `has_space` — which the blocking read and write
wrappers call per attempt (`kernel/src/syscall/io.rs`) — every
`PipeReader`/`PipeWriter` clone and close, and the read and write watches. A
compositor client, netstack, logkeeper and the shell all contend the same word.

## Impact, stated at its real size

This is **not** the device-I/O class. The copy is memory-speed and takes no
device round trip, so it does not approach `sync::Lock`'s 500,000,000-spin
`DEADLOCK` panic the way the kernel's block device lock did before the block
layer left the kernel.

What it is: a global lock held with preemption disabled for a stretch that
scales with a userland-chosen length up to 2 MiB, on the path that carries every
IPC message in the system. The scheduler's own bar for an uninterruptible
stretch is `MAX_PASS_NS = 200_000` ns (`kernel/pure/sched/cpu.rs`); a 2 MiB copy
plus, on first write, a 2 MiB zeroing and a bitmap scan is on the wrong side of
that at any plausible memory bandwidth. **The hold time is not measured and that
is the first thing owed.**

Two things make it worth closing rather than noting. The convoy is machine-wide,
not per-pipe: one process reading 2 MiB out of a full pipe stalls an unrelated
process's `SYS_CONNECT` and an unrelated reader's readiness check. And it is
directly userland-driven — the length is the caller's, so a program that wants
the stall only has to ask for it.

## Precondition / repro

1. Create a pipe or connection; write until the ring is full (2,097,088 bytes).
2. From the reader, `SYS_READ` with a 2 MiB buffer.
3. From another CPU, time an unrelated `pipe::has_data` or `pipe::create`; it
   waits for the whole copy.

For the allocation half: `SYS_CONNECT` then a single first `SYS_WRITE` of one
byte runs the scan and the 2 MiB zeroing under `PIPES` before one byte moves.

## Fix direction

`PIPES` is doing two jobs and only one of them needs the global. It is the table
lock, and it is also the serialization `Ring` relies on — `ring.rs`'s doc says
"Every access is serialized by the caller's lock (`kernel::pipe`'s `PIPES`)",
which is why the cursors are plain integers rather than atomics. So the copy
cannot simply be lifted out; the ring's data-race argument has to move with it.

The shape that keeps that argument true is a per-pipe lock under the table lock:
`PIPES` guards the `IdMap` and the refcounts, a `Lock` (or a claimed-range token)
inside `Pipe` guards `Backing`, and the table lock is dropped once the per-pipe
guard is in hand. `try_read`/`write` then hold only the pipe's own lock across
`Ring::read`/`Ring::write`, and `has_data`/`has_space`/`create`/`clone`/`close`
stop queueing behind a stranger's copy. `Ring`'s doc sentence changes with it.

The lazy allocation should move out from under both: allocate the ring page
before taking the table lock (or, on the miss, drop and retry the way
`file_cache::read_page` does), so neither the bitmap scan nor the 2 MiB zeroing
runs with a global held.

Sequencing note: this touches `sdk/abi/src/ring.rs`'s doc contract, so it lands
as one commit over `sdk/abi/src` and `kernel/src` together.

## Two checks

* **Negative control:** revert the whole change onto the base and re-run the
  contention measurement; the per-CPU wait on an unrelated pipe operation must
  return to the pre-change value.
* **Independent oracle:** `kernel-loom`'s lock models for the new two-level
  acquisition (table then pipe), which is a different epistemic source from the
  kernel's own test suite; plus `audio_tone`'s worst-wake reading, a recorded
  real-failure oracle rather than a re-derivation.
