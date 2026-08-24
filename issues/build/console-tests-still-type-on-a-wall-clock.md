---
status: open
kind: defect
opened: 2026-08-24
---

# Every console test but one still types on a wall clock

`QmpInput::type_text` sends a character every 15 ms and never asks whether the
guest took the last one. QEMU's PS/2 keyboard queue holds 16 set-1 bytes and
drops the seventeenth silently, one byte at a time, so a guest that does not
drain for a couple of hundred milliseconds receives the line with a hole in it —
and the test then asserts on a command the guest was never asked to run.

`screen_console_panic` was that twice on CI and is fixed: `console_type_line` in
`tests/toyos.rs` sends bursts no wider than `QEMU_PS2_QUEUE` and waits for the
panel to echo each one. Staged at the limit — the whole line in one
transmission, which is what a guest that drains nothing sees — the unfixed path
is 5 of 5 red and the fixed one 0 of 5 (dev host, 2026-08-24).

**Still on the wall clock**, every one of them a Fast-tier name that can dequeue
a merge:

- `screen_console_clear` — types `test_rs_test_screen_graffiti` and then
  `clear`. Its own known-red row (`src/redlist.rs`) reads *the graffiti actuator
  did not reach the panel: 0 of 2073600 pixels are [0, 192, 0]*, which is what a
  mangled command name produces, and its write-up is
  `issues/diagnostics/the-panel-once-took-no-pixels-for-two-minutes-under-load.md`.
  **It cannot use `console_type_line` unchanged**: the second command is typed
  onto a panel the graffiti actuator has painted green, so there is no legible
  prompt row to match the echo against. It needs a confirmation that survives
  the glass being overwritten.
- `screen_console_shell`, `console_locale_detect`, `desktop_locale_detect`,
  `screen_console_scroll` and the `type_line` callers under a compositor. The
  ones under a compositor have no console font to decode and need a different
  channel again.

**What this does not settle**: why the guest was not draining. The 2026-08-23
capture is a hole exactly one queue wide, so the guest read nothing from port
0x60 for the thirteen characters before it — about 200 ms — on a hosted runner
with one guest and four cores. Whether that is the host descheduling QEMU or a
gap in this kernel's own i8042 service is not decided by anything recorded, and
nothing in the tree measures the second. A bound on how long this kernel may go
without reading the i8042 would decide it; there is no such measurement today.
