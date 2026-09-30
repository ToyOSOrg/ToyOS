---
status: open
kind: defect
opened: 2026-09-27
---

# `screen_diag_boot` finds no `i8042:` line on the panel

`screen_diag_boot` fails with `"i8042:" is not on screen five
seconds after the boot finished`. The decoded panel ends at `Boot: complete`,
and no i8042 line is painted in the five seconds after it. The i8042 health
line (`report_health`, `to_screen`) never reaches the panel on the diag image.

- Green on main's nightly at 3f46a019 (run 36111884575).
- Red at e8d7c9c0 (run 36228604597).
- Red at fd62f567 (run 36278449733, guest (9)).
- Red alone at 9583d913 merged with 5e446e5c: `cargo test --test toyos-build
  -- --nightly screen_diag_boot` EXIT=1.

The range 3f46a019..e8d7c9c0 holds fourteen landings and is not bisected.
The candidates that touch the idle path, the console or the log are #502
(a288537b), #492 (b0adc600) and #506 (265a0fce).

**Exit**: the landing named by a build at it and at its parent, and
`screen_diag_boot` green on a nightly.

**Its test is deleted**, as a red test nobody has a fix for is: `8bf3fc24d`
took `screen_diag_boot` out, and `git revert 8bf3fc24d` brings it back.
