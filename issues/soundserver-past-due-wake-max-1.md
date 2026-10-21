---
status: open
kind: defect
opened: 2026-08-08
---

# soundserver's null sink spells a past-due wake `.max(1)`, which is a one-nanosecond timer

The null sink's wait in soundserver's mix loop (`system/soundserver/src/mix.rs`)
computes its poll timeout as `next_period_ns.saturating_sub(now).max(1)`, the
`1` there because `0` is the kernel's non-blocking sentinel. But non-blocking
is exactly what a grid point already in the past wants, so `0` is the right
answer and `1` is a park on a deadline that has passed. A device's wait no
longer does this: `armed` arms the next future grid point when the predicted
one has passed. soundd had the pattern in both, and it was the trigger on the fifth of the
eight T14 boots captured 2026-08-08 while chasing the cpu0 stall, where cpu0 and
cpu1 both stopped within 100 ms of `soundd: resumed`.

Not fixed here, and deliberately: it is one line in the mix loop, `apic::OneShot`'s
floor makes it safe, and what it changes is when soundd wakes on a late period —
audio timing, which is the owner's call. With the floor, a past-due grid point now costs up to
10 µs of extra lateness against a 2.9 ms period.
