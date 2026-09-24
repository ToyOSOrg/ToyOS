---
status: open
kind: tooling
opened: 2026-09-13
---

# The cable judge spends two premises nothing has measured

`tests/common/lan.rs`'s `on_metal` decides whether a ping the metal loop saw was
this boot's, and two of its steps rest on readings nobody has taken.

**The T14's RTC is unchanged across the reset.** `Driver::wire` reads
`date -u +%s` under Ubuntu before the flash and
`bootlog::host_second_inside_this_boot` spends that offset on records ToyOS wrote
after it; a machine whose firmware or whose kernel moved the counter would be
judged against a clock that no longer exists. Closed by one boot: a ToyOS
record's wall clock read back against `date -u +%s` on the machine after it, with
the loop's own skew applied — or by the judge ceasing to compare the two
operating systems' clocks at all.

**The router repeats the lease across the two operating systems.** The same
judge refuses a boot whose leased address is not the one the loop pinged, and
the one the loop pinged is what Ubuntu held on that MAC. A server that hands the
MAC a different address under ToyOS reds the arm for a fact about the router
rather than about the boot — a red naming the wrong thing, not a false green.
Closed by a lancase run whose lease record and whose `ping_addr` are compared,
which is the first thing that run prints.
