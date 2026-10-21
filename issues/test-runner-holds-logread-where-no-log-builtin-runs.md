---
status: open
kind: defect
opened: 2026-09-28
---

# test-runner holds `logread` where no log builtin runs

test-runner has no builtin that reads the log any more: `log-gate`,
`log-storm` and `log-close` are gone from `tests/guest/test-runner`. Its
`logread` is used only by the children it hands a `SysCap` duplicate to, and
it hands one only where its row also grants `dup` (`run_one`,
`tests/guest/test-runner/src/main.rs`).

- `tests/images/testcases.toml`'s row grants `dup` beside `logread`, and its
  children `disk_backtrace` and `inbox_log_post` read the log through the
  duplicate. Its comment still gives "the log gate" as the reason.
- `tests/images/metalcase.toml`'s row grants `logread` and no `dup`, so no
  child receives it and test-runner reads no log itself: nothing on that boot
  uses it.

**Evidence:** `rg -n logread tests/images` and `rg -n 'log_read|cursor_words'
tests/guest`.

**Exit:** every `logread` in a test manifest is one a program on that boot
uses, and the row's comment says which use it is for.
