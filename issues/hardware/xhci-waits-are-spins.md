---
status: open
kind: defect
opened: 2026-08-03
---

# The xHCI driver's waits are spins with preemption disabled, wherever they run

`bdf2596` moved the *boundary* — an input read no longer drives the driver — so
the only thread that runs enumeration and recovery now is the one inside
`drain_irqs`. That fixes who pays; it does not change what is paid.

Every wait in this driver is a spin against a wall-clock deadline, taken while
holding `XHCI`, which is a ticket spinlock and therefore preemption off for its
whole life:

- `settles()` — controller halt, HCRST, CNR, R/S, and the port reset. Bound
  `USB_TIMEOUT_NS`, 2 s.
- `wait_command()` and `wait_transfer()` — every command and every transfer.
  Same bound.

**The port machine took the two that ran inside a scheduler pass out of that
list.** A teardown's Disable Slot and an endpoint recovery's three-in-a-row
(Reset or Stop Endpoint, Set TR Dequeue, CLEAR_FEATURE(HALT)) are
submit-and-return now, so the six seconds above are reachable only from the boot
path and from `storage_read`/`storage_write` — the first has no scheduler to
give a pass back to, and the second is the case named below that this conversion
does not fix.

So a worst case is a CPU that does not reschedule for **six seconds**, and an
ordinary hot-plug enumeration on the T14 is ~14 ms of it (`hotplug-blocks-a-scheduler-pass`).
Nothing in the suite can measure the bad case: QEMU answers every one of these
in microseconds, which is why a driver built entirely out of them passed
everything here for a season.

**The hot-plug half of that conversion has landed**: enumeration is
`device::begin`/`stepped`, submit-and-return, driven by the port machine — so
no runtime path blocks a scheduler pass on it any more. `restart_endpoint`'s
half is done the same way: the route is `toyos_xhci::recovery`'s, driven twice
— a blocking loop for a disk's bulk pair, which runs on the thread that
faulted, and a stepped one for HID.

One case is *not* fixed by that and needs its own answer: `storage_read` and
`storage_write` are called by the page cache on a faulting thread, so a thread
touching a file on a USB disk drives a SCSI command under the same lock. The
input poll was gratuitous and could simply be deleted; this one is inherent, and
the choice is between an I/O thread and making the block layer asynchronous.
