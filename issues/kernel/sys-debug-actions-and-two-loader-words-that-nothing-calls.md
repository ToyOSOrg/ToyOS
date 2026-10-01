---
status: open
kind: defect
opened: 2026-10-01
---

# `SYS_DEBUG` actions and two loader words that nothing calls

The guest suite's first cut
(`issues/build/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`) deleted the only
callers of ten `SYS_DEBUG` actions and the loader's reading of two boot words, and left their
names in `toyos-abi/src`, because a deletion there is an ABI change and that pull request was not
one. Each kernel arm still answers its action. `git grep -w <name> -- tests userland toyos src`
finds no caller of:

- `debug_action::PANIC` (0), `NULL_READ` (1), `LOCK_ACROSS_SWITCH` (2), `HEAP_OVER_CEILING` (6)
  and `IDLE_GUARD_READ` (9), which `test_panic_child` took by number for stage B's
  `syscall_panic_halts`, `syscall_fault_halts` and `heap_over_ceiling_halts` and stage C's
  `idle_stack_guard` and `lock_across_switch_halts`;
- `HEAP_AT_CEILING` (5), `HEAP_AT_CEILING_PAGE_ALIGNED` (7) and `LOWER_SYSINFO_BOUND` (19),
  `heap_ceiling`'s, for stage C's `heap_ceiling_bounds`;
- `SCREEN_GRAFFITI` (8), `test_screen_graffiti`'s, for stage G's `screen_console_clear`;
- `LOG_PATTERNED` (21), test-runner's `log-gate` and `log-storm`, for stage C's
  `log_conservation_smp2`;
- `boot::WRITE_NO_LAYOUT_PARAM` and `boot::WITHHOLD_ROOT_PARAM`, for stage A's
  `kernel_args_layout_refused` and `root_withheld_refused`.

Owner: the orchestrator.

**Exit**: for each name, `git grep -w` finds a caller again, its stage's row, or an ABI change
retires the name with its kernel arm and never reuses its number.
