---
status: open
kind: defect
opened: 2026-09-30
---

# `SYS_DEBUG` action 8, `SCREEN_GRAFFITI`, has no reader

`screen_console_clear` was the one test that asked for it, through its
`test_screen_graffiti` guest binary, and it is deleted as a filed flake
(`issues/build/parallel-tests-red-under-other-suites.md`). What is left reads
nothing: `toyos_abi::syscall::debug_action::SCREEN_GRAFFITI`, its dispatch arm
in `kernel/src/syscall/dispatch.rs`, and `panic_console::graffiti`.

It stays because retiring a `SYS_DEBUG` action is an ABI change, and the brief
that deleted its test authorised one for actions 17 and 18 alone.

**Exit**: action 8 retired the way 14, 15, 17 and 18 are, its number never
reused, with its dispatch arm and `graffiti` — or its test restored.
