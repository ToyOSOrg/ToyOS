---
status: open
kind: defect
opened: 2026-08-28
---

# A pipe's lock spans a 2 MiB user copy and the frames it takes for it

Each pipe owns its state behind its own `sync::Lock` (`kernel/src/pipe.rs`),
which disables preemption before it spins (`kernel/src/sync.rs`). The bulk copy
runs inside that lock, through the frame store pipes and connections share
(`kernel/src/frame_bytes.rs`):

* `FrameBytes::read` makes one walk of the caller's window,
  `buf.write_pieces`, and copies each pinned piece out of the frames it
  covers with `bytes.peek`, under it;
* `FrameBytes::write` makes one walk, `buf.read_pieces`, and copies each piece
  into its frames with `bytes.place`, in the same acquisition.

Each 16 pages of either copy take the caller's space lock and populate any
untouched page, a frame from the allocator and a 4 KiB zeroing.

A pipe's copy is bounded only by the pipe: `PIPE_CAPACITY` is 2 MiB, the
largest single copy under the lock, and nothing above caps it
(`SYS_READ`/`SYS_WRITE` pass the caller's length through).

A connection direction (`kernel/src/message.rs`) copies one whole message under
its queue's lock, so its copy is bounded at `MAX_MESSAGE_LEN`, 16 KiB, and its
frames are taken the same way. Both its send and its receive run that copy
inside `process::with_process_data`, nesting the queue's lock in the process
lock: the receive installs the message's handles in the caller's table and
copies its bytes in one hold, so a refusal takes nothing.

A write that grows the pipe also takes its frames under the same lock,
nesting the frame allocator's lock: up to 513 frames, unzeroed, under one
hold, and the heap the list of them grows into. A read that empties frames
past the spares kept gives them back under the same nesting.

What queues behind it is that one pipe's traffic: its other end's reads and
writes, `has_data`/`has_space` from its blocking wrappers and polls, and its
handles' clones and closes. The scheduler's own bar for an uninterruptible
stretch is `MAX_PASS_NS = 200_000` ns (`kernel/pure/sched/cpu.rs`); a 2 MiB copy
is past it at any plausible bandwidth. The hold time is not measured, and that
is owed first. The length is the caller's.

## Fix direction

The lock is also the serialization `kernel::pipe::Bytes` relies on, so the
copy cannot simply be lifted out: a claimed-range token per end, or a copy
bounded per acquisition, has to carry the data-race argument. The frames can
be taken before the lock and given back after it, as the read end's last
holder already gives back what the pipe held.

**Exit**: no pipe or queue lock is held across more than a bounded copy or any
allocation, the connection's nesting in the process lock included, the hold
time is read before and after, and `audio_tone`'s worst wake does not move.

This is stage S0-a of `issues/anonymous-memory-swaps-to-an-encrypted-partition.md`.
