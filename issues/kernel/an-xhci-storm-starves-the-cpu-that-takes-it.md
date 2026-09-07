---
status: open
kind: defect
opened: 2026-09-08
---

# An xHCI interrupt storm starves the CPU that takes it out of its own timer

T14 run 25's own census, over the 34 s the stick was being written
(`/Users/jan/.claude/jobs/2280e09e/tmp/t14-run20/metaldevicecase.log`, the
`irq:` block at 34.609 s):

    irq: cpu0 total=103171 timer=10 xhci=103161 ...
    irq: cpu1 total=25     timer=25 xhci=0
    irq: cpu3 total=989    timer=989
    irq: cpu4 total=139    timer=139

**cpu0 took ten timer interrupts in thirty-four seconds** and a hundred
thousand xHCI ones. The same span carries

    LOCK CONTENTION: 50M spins at src/vfs.rs:32:18, ticket=38 now=37

so another CPU held the VFS lock across a stick write for seconds at a time.

`arch::idt::xhci`'s handler stamps `irq_ring` and EOIs; the work is
`xhci::poll_if_pending`, which `try_lock`s `XHCI` and **returns doing nothing**
when the lock is held. Nothing masks the source in between, so every event the
controller produces while that lock is held is one more interrupt on the CPU
that cannot service it. The record stays set, the CPU re-enters a pass, declines
the lock again, and the cycle costs it the interrupt budget it needed for its
own timer.

That is a CPU making no progress while looking busy, and it is the state
`crate::deadline`'s poll relies on *some* CPU escaping.

**What would fix it**: the interrupter's `IMAN.IE` masked when the poll declines
the lock and cleared by whoever takes it — so a controller whose driver is busy
raises one interrupt and not a hundred thousand — or an event-ring drain that
does not need `XHCI` at all.

**Not this task's**: found while root-causing the loaded-host hang whose cause
was the timer arming and not this. Whether an xHCI storm can by itself hold a
CPU out of its timer for the whole of a bound is unmeasured — and it is one of
the shapes `a-job-list-hangs-with-interrupts-on-and-the-deadline-ends-it.md`
leaves open.
