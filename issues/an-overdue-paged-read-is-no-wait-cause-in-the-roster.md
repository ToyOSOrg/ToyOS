---
status: open
kind: finding
opened: 2026-10-31
---

# An overdue paged read is no wait cause in the roster

A holder's read past its pager's report time is counted and logged overdue
(`kernel/src/object/paged.rs`, `Overdue`), and waits on. The roster shows its
thread waiting in `WaitClass::Io`, like any other I/O, so a reader held by a
slow pager is told apart only by the log line. Memory design §7.8 asks for
the overdue read as the thread's wait cause.

**Exit**: the roster names an overdue paged read as its thread's wait cause.

**Owner**: the process-memory track
(`issues/process-memory-in-4-kib-pages-still-copies-each-programs-text.md`).
