---
status: open
kind: defect
opened: 2026-09-25
---

# A netd that dies while serving leaves the host's log stream silent

A connection on `logd`'s port lives in netd's TCP state, and a netd that ends
takes it with no FIN and no reset: the host's side waits for a byte that no
process on the machine can send. The one death this side is told of first is a
swap init has accepted — `logd` turns readers away from init's `accepted` until
the old netd is gone, and `metalswap::swap` holds the swap's go until `logd`
has said so (`toyos_logstream::CARRIER_LEAVING`), then redials.

Every other death is unannounced: a replacement that passes its first moments
and then ends inside probation (init's `failed` and `restored` are then said
into a connection already gone), and a netd that crashes outside any swap. In
both the host's `Stream` reads nothing more, and `metalswap::swap` reaches its
window with init's final word missing — red, and loud, but about the stream
rather than the swap.

No test stages either: `swap_crash_rolls_back`'s replacement panics before it
serves anything, so the host's redial is admitted by the restored netd.

Exit condition: a host reader that learns of an unannounced netd death through
an event — the replacement netd ending the connections it cannot know, or a
stop that ends them before netd goes — with a test whose replacement serves and
then ends inside probation.
