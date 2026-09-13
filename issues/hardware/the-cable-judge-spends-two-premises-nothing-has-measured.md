---
status: open
kind: tooling
opened: 2026-09-13
---

# The cable judge spends two premises nothing has measured

`tests/common/lan.rs`'s `on_metal` decides whether a ping the metal loop saw was
this boot's, and two of its steps rest on facts no run has taken a reading of.

**The T14's RTC is unchanged across the reset.** `src/metal.rs`'s `Driver::wire`
reads `date -u +%s` under Ubuntu before the flash and
`bootlog::host_second_inside_this_boot` spends that offset on records ToyOS wrote
after it. Nothing has measured that the two operating systems read the same
counter to the second, and a boot whose firmware or whose kernel moved it would
be judged against a clock that no longer exists. It would take one boot to
measure: a ToyOS record's wall clock read back against `date -u +%s` on the
machine after it, with the loop's own skew applied. Closed by that reading, or
by the judge ceasing to compare the two machines' clocks at all.

**The router repeats the lease across the two operating systems.** The same
`on_metal` refuses a boot whose leased address is not the one the loop pinged,
and the address the loop pinged is the one Ubuntu held on the same MAC. On a
server that hands the MAC a different address under ToyOS the arm reds for a
fact about the router rather than about the boot — which is a red that names the
wrong thing, not a false green. Closed by a lancase run whose lease record and
whose `ping_addr` are compared, which is the first thing that run will print.

Neither premise has an owner today: both were recorded in a pull request body,
which nothing reads back.
