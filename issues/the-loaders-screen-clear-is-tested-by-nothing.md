---
status: open
kind: tooling
opened: 2026-11-05
---

# The loader's screen clear is tested by nothing

`loader/src/main.rs` clears the firmware's console before its first line
(`ClearScreen`, UEFI 2.11 §12.4.8), so the panel holds the loader's lines and
none of the firmware's. `screen_loader_clears` checked that on QEMU by holding
the machine at a loader pass that boots no kernel: the pass after a reset that
read the black-box page. That pass is gone with the page, every pass now boots
a kernel, and the kernel's first paint replaces the loader's screen before any
poll can be sure to catch it, so the test was deleted.

Owner: the loader.

**Exit**: a guest test holds a loader pass's screen without racing the
kernel's first paint, and reds where `clear_screen` is not called.
