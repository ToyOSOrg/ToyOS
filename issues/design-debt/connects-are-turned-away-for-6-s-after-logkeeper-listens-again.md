---
status: open
kind: defect
opened: 2026-09-28
---

# Connects are turned away for 6 s after `logd` listens again

In a swap of netd, connects to `logd`'s port keep being turned away for about
6 s after `logd` itself has already re-bound the port and init has said the
new netd is in service. In a hold-red run at PR #566's `7e06a657`, netd
re-bound `41337` at 1.758 s and init said `in service` at 6.739 s
(`566r2-hold-red.log:330`, `:332`); the matching hold-green run (guest-
identical, since the branch's fix is host-only) only had its redial admitted
at 7.721 s (`566r2` hold-green oracle line). That is what makes the redial's
window long after a swap — not `logd`'s closed listener during the swap
itself, which is announced and handled.

## Exit condition

Whatever holds a bound listener from accepting for those ~6 s is named, and
either removed or bounded by a measured floor — shown by a run whose
admission time tracks `logd`'s own re-bind and init's `in service`, not
trailing it by seconds.
