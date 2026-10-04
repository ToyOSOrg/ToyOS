---
status: open
kind: track
opened: 2026-07-30
---

# The diary computes no lateness, and records no window, shootdown or slow system call

`kernel/src/trace.rs` is the diary: a ring per CPU of 32-byte
`toyos_abi::trace` records, written always through the log's slot protocol
and stamped with the CPU's raw counter, by the timer, the scheduler,
`irq_ring` and every `kernel::sched::hw::TraceEvent` the core emits.
`SYS_TRACE_READ` reads it on the `trace` right, `toyos-trace` decodes it, and
`/system/bin/trace` prints it. Nothing reads a lateness off it, and nothing
samples RIP.

Part of `issues/toyos-explains-itself.md`.

**Proposal accepted** (owner, 2026-10-03, "Build it as proposed"): the ring
is finished in three steps, and the work then stops and is judged before
anything more is built on it.

1. **The ring is readable**: the log's slot protocol, raw counter stamps,
   `SYS_TRACE_READ` in the shape of the log's read, the `trace` right it
   needs, and a decoder crate. **Exit**: host tests on the decoder; a guest
   test in which a spawned child's wake precedes its pick, a second cursor is
   undisturbed, loss is counted after a flood, and a process without the right
   is refused and lives; and on the T14, the ticks a record costs, measured by
   a boot that writes a million records. The refusal clause is the owner's
   ruling (2026-10-04), **"Refuse, like others"**: "Keep one rule for every
   permission: the request is refused and the program lives. The track's exit
   is reworded to that. If you want missing permissions to end programs, that
   becomes one change for all of them." Built: a capability without `trace`
   is refused with `PermissionDenied` and its caller lives, the LLDB reading
   path and its pinned numbers are gone, and a record costs 53.15 counter
   ticks on the T14 (`trace_record_cost` at `70c1fbf56`: 1000000 records in
   53145868 ticks).
2. **Timer and thread lateness**, computed by a reader from the arm, fire and
   pick events, with no tracer in the kernel. **Exit**, on the T14: the 50 ms
   for which `hold_once` keeps both windows open once a boot
   (`kernel/src/windows.rs`) reads back as at least that much lateness.
   A deadline's wake is recorded as a `timer-fire` naming no task
   (`fire_deadlines` in `kernel/pure/sched/cpu.rs`), so a reader cannot say
   which sleeper a fire woke; and a `wake` is the owning CPU's handling of a
   post, so the span from a post on another CPU to it is in no record.
3. **Windows over a threshold, shootdown records, and slow system calls.**
   A system call over the threshold is recorded with its number and its
   program, in the shipped kernel: **"Always on"** (owner, 2026-10-03).
   **Exit**: `SYS_DEBUG`'s
   shootdown-acknowledgement delay names the delayed CPU and its time in the
   trace. A system call held past the threshold in a shipped-configuration
   kernel reads back from the trace with its number and its program. A
   window's record names its opener by address, which
   `issues/the-cpu-that-spawns-a-toybox-applet-reads-1-4-ms-of-preemption-off-on-the-t14.md`
   and
   `issues/the-supervisors-claim-of-a-pci-function-the-t14-lacks-holds-preemption-off-for-3-8-ms.md`
   need.

**Ruled** (owner, 2026-10-04), on when these steps start, **"Both in
parallel"**: "Start the trace diary's first steps now, beside latency step 1
(interrupts on in system calls). Step 1 is already measured by existing T14
rows. The tail delays are only worked on once the diary shows their cause."
The step he names is step 2 of
`issues/toyos-beats-linuxs-latency-on-the-t14.md`,
`issues/syscall-preemption-is-incidental.md`; that mapping is the
orchestrator's, not his.

Behind that judgement, and not before it: interrupt enter and exit records
with a noise reader, the profiling sample, and the censuses moved onto the
ring.

The proposal's other lines, accepted with it:

- The ring stays always written in the shipping kernel.
- Reading it needs a new `trace` right.
- The LLDB reading path and its pinned numbers go.
- A T14 judge may read a binary trace file, the decoder printing the lines a
  pull request quotes.
- The profiling sample shares the ring.
- 2 MiB of rings at eight CPUs is accepted.

The reader ships with ToyOS, in the owner's words (2026-10-03): "its obvious
that we want to be able to understand kernel metrics from within toyos ...
thats an important and productive tool that should be shipped with toyos."
