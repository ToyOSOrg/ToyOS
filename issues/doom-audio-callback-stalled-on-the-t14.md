---
status: open
kind: defect
opened: 2026-08-05
---

# Nothing explains why doom's audio callback stalled on the T14

doom's sound producer can no longer kill the game when its callback stops
consuming, and that is the whole of what is now known. **Why the callback stopped on the
owner's machine, about five seconds into play, is not.** The evidence is one
abort message, because the process that would have carried the answer is the
one that died.

An audio client's RT standing is *lent*, not held: soundserver's mix thread
takes the RT band (`mix_thread`, `system/soundserver/src/mix.rs`), and every
pipe write it makes lends the woken reader a one-quantum window
(`wake_pipe_readers`). **On a machine with no audio device none of that
happens.** The null sink deliberately does not request the band — it protects
no audible output — so `driver::current_is_rt()` is false at its
`signal_clients` write and the client's callback thread is woken as an ordinary
thread. The T14 had no audio device ToyOS drove when doom stalled there, so the
one thing that keeps a 2.9 ms deadline met was absent. soundserver now drives
the T14's HDA (the `hda_tone` metal row), so a doom run there today takes the
band and no longer reproduces that condition.

That is a mechanism, not a measurement, and two others are equally live: the
`drain_irqs` entry, where any syscall on that thread could become the
kernel's USB driver's engine for as long as a second, which this tree no
longer has (the driver is usbd's); and plain scheduling pressure from a
game thread and a compositor that never yield to it.

What would decide it is the callback's own period count against wall clock, on
that machine.
