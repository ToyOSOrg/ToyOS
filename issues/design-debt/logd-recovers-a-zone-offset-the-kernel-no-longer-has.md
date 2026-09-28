---
status: open
kind: defect
opened: 2026-09-28
---

# logd recovers a zone offset the kernel no longer has

The RTC is UTC (owner ruling): the kernel reads no zone, and
`SYS_CLOCK_REALTIME` and `SYS_CLOCK_EPOCH` answer from one UTC anchor
(`kernel/src/clock.rs`). `userland/logd/src/wall.rs` still recovers an offset
from the two calls through `toyos_wallclock::resolve`, and refuses a date in the
12-to-14-hour band — machinery for an offset that is now always zero.

**Exit condition**: logd names its file from `SYS_CLOCK_EPOCH` alone, and
`toyos_wallclock::resolve` and its gate go with the recovery.
