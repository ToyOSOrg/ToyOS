---
status: open
kind: defect
opened: 2026-09-14
---

# A 120000 ms boot deadline fired 132859 ms late, with cpu0 deep in an xHCI recovery loop

`kernel/src/deadline.rs:1-2` states the whole promise of this mechanism: "the
one bound on this machine that nothing running on it can hold off." `:11-13`
says how: armed off the parameter line and "polled from the timer interrupt
entry, on every CPU, in both rings" — "its whole requirement is that some CPU
still takes an interrupt." The bound itself is checked in `poll`
(`kernel/src/deadline.rs:194-199`): one relaxed load of `AT_TSC` and an
`rdtsc` compare, called from the timer entry.

A T14 boot armed with `boot-deadline=120000` measured otherwise. The black
box `expire` seals read:

    the boot deadline expired: a bound of 120000 ms, reached at 252859 ms, with this machine in `complete`.

252859 ms is 132859 ms past the 120000 ms bound the image was armed with —
over two minutes late, not the one tick a poll running on every CPU's timer
entry should cost. The same ring tail's own `irq:` census reads:

    irq: cpu0 total=1741 timer=9 xhci=1729 userdev=0 sound=0 i8042=0 dmafault=0 hda=0 tlb=0 nmi=3 spurious=0 unclaimed=0

nine timer ticks against 1729 xHCI interrupts on cpu0. In the same window the
ring tail carries the mass-storage recovery loop that produced them, ending in:

    [3.556 cpu0] xHCI: 00:14.0 slot 5 endpoint 3 is Stopped, recovering
    [5.556 cpu0] xHCI: Set TR Dequeue timed out

a two-second wait between the stop and the timeout that follows it, on the
same CPU whose timer count the census names as nine.

## What would answer it

A test that arms the deadline, drives one CPU into the same shape — many
interrupts serviced, few timer ticks taken — and asserts the machine still
reaches `expire` within one timer tick of the armed bound regardless of what
that CPU is doing, rather than however much later some other path happens to
notice.
