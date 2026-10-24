---
status: open
kind: defect
opened: 2026-08-28
---

# A pipe's lock spans a 2 MiB user copy, and a pipe's first write allocates and zeroes 2 MiB under it

Each pipe owns its state behind its own `sync::Lock` (`kernel/src/pipe.rs`),
which disables preemption before it spins (`kernel/src/sync.rs`). The bulk copy
runs inside that lock:

* `PipeRef::try_read_within` runs `ring.read(len, |off, src| buf.write_run(off, &src))`
  under it;
* `PipeRef::write` runs `ring.write(n, |off, dst| buf.read_run(off, dst))` in
  the same acquisition.

The copy is bounded only by the ring: `PIPE_SIZE` is `PAGE_2M` less the 64-byte
`RingHeader`, so **2,097,088 bytes** is the largest single copy under the lock,
and nothing above caps it (`SYS_READ`/`SYS_WRITE` pass the caller's length
through). Each 16 pages of it take the caller's space lock and populate any
untouched page, a frame from the allocator and a 4 KiB zeroing.

A pipe's **first** write is worse: `State::back` allocates the ring page lazily
under the same lock, nesting the frame allocator's lock and a 2 MiB zeroing.

What queues behind it is that one pipe's traffic: its other end's reads and
writes, `has_data`/`has_space` from its blocking wrappers and polls, and its
handles' clones and closes. The scheduler's own bar for an uninterruptible
stretch is `MAX_PASS_NS = 200_000` ns (`kernel/pure/sched/cpu.rs`); a 2 MiB copy,
and on first write a 2 MiB zeroing, is past it at any plausible bandwidth. The
hold time is not measured, and that is owed first. The length is the caller's.

## Fix direction

The lock is also the serialization `Ring` relies on (`sdk/abi/src/ring.rs`'s
doc), so the copy cannot simply be lifted out: a claimed-range token per end,
or a copy bounded per acquisition, has to carry the ring's data-race argument.
The lazy allocation moves out from under the lock: allocate before taking it,
or drop and retry on the miss.

**Exit**: no pipe lock is held across more than a bounded copy or any
allocation, the hold time is read before and after, and `audio_tone`'s worst
wake does not move.
