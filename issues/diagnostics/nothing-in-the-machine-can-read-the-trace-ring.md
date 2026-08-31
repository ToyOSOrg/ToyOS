---
status: open
kind: track
opened: 2026-07-30
---

# Nothing inside the machine can read the trace ring, and nothing samples RIP

Rewritten 2026-08-24 from three sentences on unbuilt profiling layers 2 and 3
that pointed at a "diagnostics roadmap" in CLAUDE.md which no longer exists,
in a layer numbering unreadable without that document.
Read against the tree instead, one of the two things it said was missing is
built and the other's stated blocker is gone.

**What answers "where did this process's time go".** `SYS_PROCESS_STATS` (94)
takes a `Process` handle and fills `ProcessStats`: wall and CPU, syscall count
and total, demand and zero faults with their time, read ops and bytes, blocked
time split five ways (io, futex, pipe, ipc, other), runqueue wait, peak memory,
allocation count. `/bin/stats <command>` spawns, waits and prints it. Per-syscall
counts exist too — `ProcessData::syscall_counts`, 128 bins — and nothing reads
them out. What is still owed there is who may ask:
`issues/diagnostics/the-kernel-keeps-nothing-it-enumerates.md`, a policy gap now rather
than an ABI one, because no diagnostic tool holds a handle to a daemon.

**Event tracing is built, and only a debugger can read it.**
`kernel/src/trace.rs` is a per-CPU ring of `RING_CAPACITY` 24-byte `repr(C)`
records, enabled from `main.rs` at boot, written by the APIC timer, the IDT, the
scheduler, `irq_ring` and every `toyos_sched::hw::TraceEvent` the core emits —
pick, idle, preempt, block, wake, timer arm/stop/fire, IRQ entry, and IRQ drain
carrying its measured service latency. Its one reader is LLDB: `p &TRACE_RINGS`
and then `memory read`, which is why the discriminants are fixed by hand and
held there by const assertions. So a wedged kernel under a debugger can be asked
what the schedule was, and a booted machine can ask itself nothing at all — no
syscall, no tool, no gate. **That is what is to be built**, and the ring is the
expensive half of it, already paid for.

**RIP sampling is not built, and the blocker `trace.rs` names for it is gone.**
That header says layer 3 "builds on this ring once in-kernel call-stack
unwinding is available". It is available: `arch/idt/exceptions.rs` walks the
`rbp` chain for kernel and for user frames, with a fault-tolerant variant for
the double-fault path, and `symbols.rs` resolves each frame against the loaded
symbol table with the elision `toyos-elide` decides.

**Residual, one line**: `kernel/src/trace.rs:13` still cites "the diagnostics
roadmap (see CLAUDE.md)", which points at a deleted document.
