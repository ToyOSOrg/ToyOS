---
status: open
kind: defect
opened: 2026-09-25
---

# A console's watch waits on the keyboard, and its readiness is the serial line

A blocking read of a console was moved onto the serial line's own wait; a
watch through an inbox (`Poller::watch` of a console handle) was not, so
serial input never completes it: a program that polls its console for input
on a machine whose console is a serial port is not woken by a key typed there.

## Exit condition

A console's poll source and its readiness are the same source, and a test that
watches a console handle through a poller and types on the serial line sees
the watch complete.
