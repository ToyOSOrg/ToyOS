---
status: open
kind: tooling
opened: 2026-09-23
---

# A service swap on the T14 lives inside a metal boot's bound

`toyos-metal --swap <service>` replaces a service's binary on a running T14
with no flash and no reboot. What there is to swap *on* is a metal-staged boot,
and every metal image carries `boot-deadline=` (`toyos_tco::WEDGE_BOUND_MS`,
120 s) and a runner whose list ends in `reboot` (`toyos_tco::JOB_BOUND_MS`,
60 s). The swapping boot (`lanswapcase`) holds on `lan_swap_hold`, which lasts
until the swap invocation hands the machine back with `reboot` — the runner's
60 s bound standing behind it.

So the loop the owner asked for — netd rebuilt on the Mac and swapped in, in
seconds, over and over, for weeks of LAN work — has on the T14 today the span
from netd's first lease (20.9 s of boot time on run 122) to the runner's
60 s: long enough for one swap and a few, and
then a flash. Under QEMU the loop has no such bound.

Nothing here is wrong: the bounds are what keep an unattended machine from
needing a hand. The ruling: a host-renewed lease over the cable bounds a session
boot, replacing the fixed 60 s runner bound for session boots. The host renews
the lease while it holds the session, and when the lease expires the T14's
watchdog resets it, so a host that goes away hands the machine back on its own.
