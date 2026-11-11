---
status: open
kind: tooling
opened: 2026-11-11
---

# A program that stalls in `libc_children` is not named, and the driver ends the suite

`libc_children` runs 23 programs in one boot, one `run_test` each
(`tests/common/libcchildren.rs`). When one of them hangs, the row says
nothing until the driver's 900 s silence guard ends the whole suite.

Seen once, in guest-rows run 42949792797, a control that made
`libc_read_restarts` hang on purpose: the row's last line is `quit_tree`'s,
the program before it; 900 s later `build/ci.rs` ended the suite with its
process group; and a second step went red on the scratch directory the
killed suite left in `/tmp`.

Why, by reading and not by a run:

- The row prints failures only after its last program, so a stall says
  nothing when it happens.
- `run_test`'s bound ends one program's wait on the host and kills nothing
  in the guest. It is scaled by the host's boot time, and held to twice that
  while the guest keeps talking (`tests/common/qemu.rs`).
- The guest's `test-runner` reads one `run` line at a time and waits for
  that job. After a stall the next `run` is never read, so every later
  program spends its own full bound in silence.
- `build/ci.rs` says the guest harness ends a silent guest long before its
  900 s. For this row that is false.

Not in the log: whether the row's own bound had expired for the stalled
program before the guard fired. The log prints no scale and the run kept no
serial capture.

The cost is a red with no name and no console window, a runner held for
fifteen minutes, and a leaked scratch directory that reds a second step.
Other rows that call `run_test` repeatedly on one boot may share it.

**Exit**, proposed by #1214's review and not decided: the row stops at the
first program that ends with no exit code, since that boot runs nothing
more, and says which; a stalled program then costs one bound and is named.
