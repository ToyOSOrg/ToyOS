---
status: open
kind: defect
opened: 2026-09-25
---

# A console's watch waits on the keyboard, and its readiness is the serial line

A blocking read of a console was moved onto the serial line's own wait; a
watch through an inbox (`Poller::watch` of a console handle) was not. Its one
source was the kernel's keyboard watch, which went with the kernel's input
streams (the small-kernel track's step 10), so a console has no readable
watch at all now: registering one is refused, and libc's `poll` reads every
completed token as ready, a refusal included (`sdk/libc/src/posix_io.rs`), so a
program that polls its console for input spins rather than waits.

## Exit condition

A console's poll source and its readiness are the same source, and a test that
watches a console handle through a poller and types on the serial line sees
the watch complete.
