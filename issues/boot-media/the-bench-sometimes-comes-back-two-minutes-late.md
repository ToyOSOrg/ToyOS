---
status: open
kind: finding
opened: 2026-09-27
---

# The bench sometimes comes back two minutes late

`bench_loop_drives_a_toyos_machine` prints `the bench gave back its /log over
the runner key <n> s after it went down`. Over 48 runs on the dev host (QEMU
11.1.1), n was 13 to 34 s in 41 of them and 126 to 132 s in 7: 3 of 23 at
`4def9c86` with a log line per netd listener state, and 4 of 25 with netd's
listener fix. The extra time is close to the 120 s bound
`tests/ssh-client-host` puts on a whole run, which would fit one `fetch` in
`metalbench::Bench::wait_for_the_log` that connected while the machine was
rebooting and then waited out that bound. That has not been checked.

**Exit**: the cause measured, and either the late runs gone or the wait they
spend recorded where it is spent.
