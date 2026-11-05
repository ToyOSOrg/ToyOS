---
status: open
kind: defect
opened: 2026-08-28
---

# A pipe's or queue's lock hold is unmeasured, and a queue still grows under it

A pipe (`kernel/src/pipe.rs`) and a connection direction
(`kernel/src/message.rs`) each keep their state behind a `sync::Lock`, which
disables preemption before it spins (`kernel/src/sync.rs`). No user copy and no
frame allocation runs under either any more: a read or a write claims its bytes
under the lock, copies them and takes or gives back frames with it let go, and
ends its claim under it (`kernel/src/frame_bytes.rs`, `kernel::pipe::Bytes`).
A hold is a claim's arithmetic, one chunk's runs (at most 17), a list moved
into a longer one (at most 513 words), or a batch of 32 spare frames listed.

What is left:

* **The hold time has not been read**, before or after. Before, a pipe's lock
  spanned up to a 2 MiB copy and 513 frames from the allocator; the scheduler's
  bar for an uninterruptible stretch is `MAX_PASS_NS = 200_000` ns
  (`kernel/pure/sched/cpu.rs`). The bound above is by inspection. QEMU's clock
  cannot read it; a bench can.
* **A connection's queue grows its two lists under the lock.**
  `kernel::message::Queue` keeps its heads and its handles in `VecDeque`s that
  start empty and double as `push` fills them, a heap allocation under the
  queue's lock, up to `MAX_QUEUED_MESSAGES` and `MAX_QUEUED_HANDLES` entries.
  Their bound is charged when the connection is made, so reserving it then is
  the direct fix, at that memory per direction whether or not it is used.
* **`audio_tone`'s worst wake** has not been read against the change.

**Exit**: the hold time read on a bench before and after, the queue's lists
grown outside its lock or reserved, and `audio_tone`'s worst wake unmoved.

This is stage S0-a of `issues/anonymous-memory-swaps-to-an-encrypted-partition.md`.
