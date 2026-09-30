---
status: assigned
kind: defect
opened: 2026-09-30
---

# An IrqWatch's freeing cancel compiles in a handler

Held by the small-kernel track's stage 6
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`).

An interrupt handler may not free: it can interrupt the allocator's holder.
`IrqWatch` has no `post`, so a thread's post written in a handler is refused
at build. But `cancel_polls` is every watch's, and it frees every entry it
takes out. Its callers are the claim's release and the close of a claim or an
audio device, both threads', and no type keeps it out of a handler.

**Evidence**: `WATCHES[slot].cancel_polls();` written after
`pcidev::note_fault`'s post builds: the x86-64 kernel clippy shape exits 0.
The kernel holds no proof of thread context a handler cannot mint. `Parkable`
proves a context may park, and with `IrqWatch`'s cancel taking one,
`Parkable::at_entry()` written at the same line in `note_fault` builds too
(clippy 0). Its only check is at run time, an assertion on the preempt
depth: by reading, a handler that interrupted Ring 3 passes it, and the
release, reached from `close_all` under the `ProcessData` lock, fails it.

**Exit**: a proof of thread context no interrupt handler can construct, taken
by `IrqWatch`'s `cancel_polls`, so the `note_fault` line above is refused at
build.
