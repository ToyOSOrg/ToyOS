---
status: expected-red
kind: defect
opened: 2026-10-01
---

# A job that exited was never reported ended

`650-libcllvm-whole.log` (`wt/toyos-libcllvm`, userland/libc alone):
`blockd_serves_partitions` — "role bench exited None" after 3559 s, ended by
hand. The bench passed and its process ended:

```
{35.962 pid=11 test-runner} blockd_io: PASS bench
[kernel 35.969 cpu0] exit: blockd pid=12 code=137 cpu=1450ms
[kernel 35.978 cpu0 tid=1] exit: test_rs_blockd_io pid=11 code=0 cpu=23247ms
```

and `===TEST_END test_rs_blockd_io exit=0===` never came. From 35.978 s to
3540 s the only lines are the kernel's own ten-second `sched:` and `PMM:`
lines, every CPU `ready=0` with its threads parked. test-runner prints the
marker after `child.wait()` (SYS_PROCESS_WAIT, parked on the process object's
watch until `publish_exit` posts it), and its line reaches the console
through logd. So either the wait missed the post, or the marker was written
and logd stopped forwarding: every userland line stops at 35.962, and the
capture cannot tell which.

logd reads its sources through a poller. #655 (`wt/toyos-winitstall`) found
that a poller can report a source readable again for data already read, and
a blocking read after it then waits forever. That shape would stop logd
silently while the kernel's lines go on. blockd finished its bench and was
killed after it, so the blockd ring-reset ordering #643 fixes is not on this
path.

**Exit**: the parked thread named — the next sighting needs a blocked-task
dump of a guest whose kernel still logs — and the defect fixed; then the row
goes.
