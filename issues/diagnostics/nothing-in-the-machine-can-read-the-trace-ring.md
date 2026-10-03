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

1. **The ring is readable**: the log's slot protocol, raw counter stamps,
   `SYS_TRACE_READ` in the shape of the log's read, the `trace` right it
   needs, and a decoder crate. **Exit**: host tests on the decoder; a guest
   test in which a spawned child's wake precedes its pick, a second cursor is
   undisturbed, loss is counted after a flood, and a process without the right
   is ended; and on the T14, the ticks a record costs, measured by a boot that
   writes a million records.
2. **Timer and thread lateness**, computed by a reader from the arm, fire and
   pick events, with no tracer in the kernel. **Exit**, on the T14: the 50 ms
   for which `hold_once` keeps both windows open once a boot
   (`kernel/src/windows.rs`) reads back as at least that much lateness.
3. **Windows over a threshold, and shootdown records.** **Exit**: `SYS_DEBUG`'s
   shootdown-acknowledgement delay names the delayed CPU and its time in the
   trace. A window's record names its opener by address, which
   `issues/kernel/the-cpu-that-spawns-a-toybox-applet-reads-1-4-ms-of-interrupts-and-preemption-off-on-the-t14.md`
   and
   `issues/kernel/the-supervisors-claim-of-a-pci-function-the-t14-lacks-holds-interrupts-off-for-3-8-ms.md`
   need.

Behind that judgement, and not before it: interrupt enter and exit records
with a noise reader, the profiling sample, and the censuses moved onto the
ring.

The lines he drew:

- The ring stays always written in the shipping kernel.
- Reading it needs a new `trace` right.
- The reader tool is not in the shipped image.
- The LLDB reading path and its pinned numbers go.
- A T14 judge may read a binary trace file, the decoder printing the lines a
  pull request quotes.
- The profiling sample shares the ring.
- 2 MiB of rings at eight CPUs is accepted.
