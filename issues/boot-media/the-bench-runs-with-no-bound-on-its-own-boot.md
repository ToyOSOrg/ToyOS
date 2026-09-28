---
status: open
kind: defect
opened: 2026-09-27
---

# The bench runs with no bound on its own boot

Every image a metal boot flashes carries `boot-deadline=`, because a kernel
that stops making progress without panicking is bounded by nothing else on the
T14: its PCH's TCO does not count. The bench is the machine's own image and
stays up between boots, so it carries none, and a bench kernel that hangs
needs a hand on the power button — as the owner's installed machine will.

**Exit**: a resident kernel's hang ends in a reset on the T14 — a watchdog
that counts there, or a bound that stands down only on progress rather than
at a deadline.
