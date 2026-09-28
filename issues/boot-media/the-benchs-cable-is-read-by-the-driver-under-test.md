---
status: open
kind: tooling
opened: 2026-09-27
---

# On the bench, a boot's cable is read back by the driver it tests

A `--nic` boot's judge holds the MAC its netd brought up to the MAC the
operating system before it held on that function. Through Ubuntu that was
Linux's `e1000e`, a second driver's reading; on the bench it is the bench's own
netd record (`metalbench::wire`) — the same driver as the boot under test, so
the comparison agrees with itself, and the lease comparison and the ping
bracket are the only independent halves left.

**Exit**: the cable's facts come from outside the machine again — the switch or
router it leases from, or the frames this host sees on the link — or the lan
judges say which of their checks the bench cannot make.
