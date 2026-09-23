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
needing a hand. What is owed is a decision: a boot staged for the swap loop
whose hold is not the runner's — longer, or held until the host lets it go
over the cable — with the kernel's deadline widened to match, and a ruling on
what bounds such a boot instead.
