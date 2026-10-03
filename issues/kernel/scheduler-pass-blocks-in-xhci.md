---
status: open
kind: defect
opened: 2026-08-05
---

# A scheduler pass may spend two seconds in xHCI before it drains its mailbox

`sched::driver::pass` and `pass_block` both open with `drain_irqs()`, and
`drain_irqs` calls `xhci::poll_if_pending()` — **before** `with_cpu(...)`, and
therefore before the CPU's mailbox drain, its deadline fires and its pick. That
call is not bookkeeping. Its own doc-comment says so:

> it enumerates hot-plugged devices and recovers broken endpoints, and both spin
> on deadlines measured in seconds while holding `XHCI`, which is a ticket
> spinlock and therefore preemption off for its whole life.

The deadline is `xhci::USB_TIMEOUT_NS` = 2 s. `cpu::MAX_PASS_NS`, the budget the
scheduler core measures a pass against in `feature = "check"` builds, is 200 µs.
The two numbers disagree by four orders of magnitude, and the driver's prologue
sits on the wrong side of the boundary the budget describes.

What a CPU inside that recovery holds is *every message addressed to it*: an
`Adopt` carrying a task, a `Wake` for a parked thread, a `Retire`. Nothing in the
scheduler can shorten it — every reap and every wake is bounded by the owning
CPU's pass latency by design, which is exactly why the design is sound.

```
retire_task: task not released after 1s: InTransit(CpuId(1))
```

That panic fired on the owner's T14 at 949.792 s of uptime with doom exiting. The
*balance*-path half of it is fixed: `hand_off` reaps a killed task rather than
handing it on, gated by simulator invariant I14. This half is not.

The second instance of the same shape used to be the idle loop's log flush;
the idle loop touches no filesystem now — the log is logd's file — and the
disk wait that survives, logd's `fsync` pinning a CPU for the device round
trip, is `issues/hardware/xhci-waits-are-spins.md`'s subject.

Closing this means making xHCI enumeration and endpoint recovery asynchronous, so
that `drain_irqs` only ever does work it can finish: drain the event ring,
dispatch HID reports, note that a port or an endpoint owes work. The debounce and
the port reset were already moved off this path for exactly this reason (CLAUDE.md,
USB hotplug); the control transfers inside `configure` and `recover_endpoints`
were not.

**And the budget cannot see it.** `cpu::MAX_PASS_NS` is measured against by
`SchedPass::finish`, from the `now` the pass was entered with to the end of
`finish_inner()` — and `drain_irqs()` runs *before* `SchedPass::begin`. The
prologue is outside the window the budget covers, so the pass-cost histogram
records a microsecond pass while the CPU had been in the driver for two seconds.
**The measured window has to start where the scheduler entry starts, and that
half of this issue is still open.**
