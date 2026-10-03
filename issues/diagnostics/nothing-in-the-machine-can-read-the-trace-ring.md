---
status: open
kind: track
opened: 2026-07-30
---

# Nothing inside the machine can read the trace ring, and nothing samples RIP

`kernel/src/trace.rs` is a per-CPU ring of `RING_CAPACITY` 24-byte `repr(C)`
records, enabled from `main.rs` at boot and written by the timer, the
scheduler, `irq_ring` and every `toyos_sched::hw::TraceEvent` the core emits.
Its one reader is LLDB: `p &TRACE_RINGS` and then `memory read`, which is why
the discriminants are fixed by hand and held there by const assertions. A
booted machine can ask itself nothing: no syscall, no tool, no gate.

**Ruled** (owner, 2026-10-03): the ring is finished in three steps, and the
work then stops and is judged before anything more is built on it.

1. **The ring is readable**: a slot protocol, raw counter stamps, a read call,
   the right that call needs, and a decoder.
2. **Timer and thread lateness**, computed by a reader.
3. **Window-over-threshold and shootdown records.**

The lines he drew:

- The ring stays always written in the shipping kernel.
- Reading it needs a new `trace` right.
- The reader tool is not in the shipped image.
- The LLDB reading path and its pinned numbers go.
- A T14 judge may read a binary trace file, the decoder printing the lines a
  pull request quotes.
- The profiling sample shares the ring.
- 2 MiB of rings at eight CPUs is accepted.
