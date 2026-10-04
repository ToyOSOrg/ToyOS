---
status: open
kind: defect
opened: 2026-10-04
---

# `screen_panic_muted` reads the panel before the report has reached its armed line

`screen_panic_muted` (`tests/toyos.rs`) takes one screendump as soon as the
panel shows `PANIC:` and then asserts the line `halt_all_cpus` paints last,
`panic: rebooting in 60 s, timed by …`. Nothing orders the dump after that
line: a guest slow between the two is read mid-report. A whole-suite run with
the host's load average at 48 (14 cores) red it with the dump ending inside
the kernel backtrace, before the armed line; the same test alone was green in
10 s.

**Exit**: the test waits for the armed line itself, bounded, and a guest
slowed between `PANIC:` and that line still passes.
