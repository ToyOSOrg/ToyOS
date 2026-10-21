---
status: open
kind: defect
opened: 2026-09-25
---

# A console's watch waits on the keyboard, and its readiness is the serial line

A console's readiness is the serial line's (`drivers::serial::has_data`),
and nothing on the serial line posts a wake. A blocking read of a console
(`sys_read`, `kernel/src/syscall/io.rs`) parks on its own thread's watch and is
woken only by its own 10 ms repoll (`CONSOLE_REPOLL`). A watch through an
inbox (`Poller::watch` of a console handle) has no source at all. Its one
source was the kernel's keyboard watch, which went with the kernel's input
streams (the small-kernel track's step 10), so a console has no readable
watch at all now: registering one is refused, and libc's `poll` reads every
completed token as ready, a refusal included (`sdk/libc/src/posix_io.rs`), so a
program that polls its console for input spins rather than waits.

## Exit condition

A console's poll source and its readiness are the same source, a blocking read
waits on it with no repoll, and a test that watches a console handle through a
poller and types on the serial line sees the watch complete.
