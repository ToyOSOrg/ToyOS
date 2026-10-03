---
status: open
kind: defect
opened: 2026-10-03
---

# `virt_mask_windows` read nine census lines and eight windows lines for one CPU

`virt_mask_windows` (`tests/toyos.rs`) judges the capture `judge_virt_job`
answers, which ends where `===TEST_END unmap_touch` arrived, with
`irqcensus::windows`: every `irq: cpu` census line has its CPU's windows line
beside it.

## Measured

A whole guest suite at `bf28c1e38`, whose `virt_mask_windows`, kernel and
`tests/common/irqcensus.rs` are `origin/main`'s at `c59e09ed6`:
`cargo test --test toyos-build`, 12 wide on the 14-core dev host at load
average 43 to 59, exited 1 with this one red in 66.8 s:

```
FAIL virt_mask_windows: census lines per cpu {0: 9, 1: 9, 2: 9, 3: 9, 4: 9, 5: 9, 6: 9, 7: 9} and windows lines per cpu {0: 9, 1: 9, 2: 9, 3: 9, 4: 9, 5: 9, 6: 9, 7: 8}: a census went out without its windows, or windows without their census
  FAIL  virt_mask_windows  (10s)
```

The whole run before it and the one after it, same tree and same command,
passed it in 8 s and 7 s. The PL011 capture of the red was not kept: the run's
`target/red-run-serial` holds the x86-64 guests' 16550 logs only.

Two candidates, neither measured: the capture ends at the marker with cpu7's
last windows line still behind it, or the kernel printed a census without its
windows.

## Owner

The windows instrument's author (`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`).

## What would close it

The cause, from a capture of a red: either the judge reads a capture that ends
on a whole report, or the kernel's report is shown to pair its two lines.
