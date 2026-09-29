---
status: open
kind: defect
opened: 2026-09-29
---

# The loader says it boots its kernel on a pass that then hands the machine back

`bootloader/src/blackbox.rs`'s foreign-record branch composes `It has been
cleared and this pass boots its kernel` in `harvest`, which `main` calls
before `attempt::read`.
When the foreign record was the only word of this image's last boot, the same
pass then prints `Boot attempts: the previous boot of this image never
reported; the machine is handed back` and boots nothing, so `loader.log`
carries two lines that contradict each other. The QEMU test
`blackbox_foreign_record` asserts the first phrase on exactly that pass.

Seen on the T14 run of `main` at `7e151819`,
the `foreignrecord` readback's `loader.log`,
second pass: the foreign-record line, then `Slot A: its image ... died on its
last boot`, then the hand-back line.

## Exit condition

A pass that clears a foreign record says what it then does, in one line that
is true of that pass, and `blackbox_foreign_record` asserts that line.
