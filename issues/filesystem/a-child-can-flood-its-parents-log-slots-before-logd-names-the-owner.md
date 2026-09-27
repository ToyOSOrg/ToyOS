---
status: open
kind: defect
opened: 2026-09-27
---

# A child can flood its parent's log slots before logd names the ring's owner

A ring keeps `CHILD_KEEP` slots for its owner only once `ring.own(pid)` has
run (`toyos/src/log/region.rs`), and logd runs it when it reads init's
registration of the ring (`userland/logd/src/origin.rs`). Until then the owner
word is 0 and a child's records take every slot. With `/log` served by fsd,
logd makes file-server round trips before it reads registrations, so a child
spawned in the first tens of milliseconds can fill the ring first.

`log_ring_keeps_the_owners_slots` measured it at b58c22c3: EXIT=1 wide and
alone, 1917 flood lines in `/log` and no `===TEST_END test_rs_log_flood
exit=0===`, logd's first line at 0.509 s against the flood's at 0.498 s. The
same test was green on the same code before the merge of 6c9e2cb2, with 1853
flood lines: which side of the race it lands on is the boot's timing.

## Exit condition

A ring's owner is named before its owner can run — by init at the spawn that
creates it, or by a registration logd reads before anything else — shown by
`log_ring_keeps_the_owners_slots` green across repeated boots.
