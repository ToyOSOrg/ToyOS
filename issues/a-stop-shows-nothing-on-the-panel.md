---
status: open
kind: defect
opened: 2026-10-04
---

# A stop shows nothing on the panel

A shutdown or reboot a press or a program asks for puts nothing on the
screen: the kernel's panel console (`kernel/src/drivers/panic_console/`) paints
only a panic and the Ctrl+Alt+D report, and on a machine without a compositor,
which is every test image, nothing else paints at all. On the T14 the owner
pressed the power button and saw nothing happen, which is also what a machine
that ignored the press looks like.

**Exit**: within a second of the supervisor's stop line, the panel shows that
the machine is stopping and which stop, on a machine with and without a
compositor, and a test reads it off the screen.
