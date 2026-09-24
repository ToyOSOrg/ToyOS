---
status: open
kind: defect
opened: 2026-09-24
---

# After `usbd` recovers from its panic, the console sometimes stops at logd's first line

**Owner:** the kernel log (`kernel/src/log/`, whose only post-boot drainer is
the `klogd` thread).
**Exit condition:** a recovered kernel-thread panic never stops the console,
and `klogd_panic_halts`' `usbd-panic` boot reaches `===READY===` on every
nightly.

`klogd_panic_halts` was red on 4 of 16 nightly runs between 09-09 and 09-18,
and on dispatch run 35357899448. Its `ALONE` re-run was green twice and red
three times. Every red is the recover arm, the `usbd-panic` boot:

```
FAIL klogd_panic_halts: "===READY===" never reached the boot console:
```

The eight red captures are in
`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/flake-census/logs/`, in jobs
102381294061, 102787750188, 103694736855 (×2), 104305462465 (×2) and
105642821649 (×2). All eight agree on the following:

- **The panic is recovered.** Each capture has `PANIC: panicked at
  src/drivers/xhci/usbd.rs:23:9`, its backtrace and `Process: usbd pid=2
  state=Live`. After that the machine carries on: logd, soundd and test-runner
  are spawned, and `init: started test-runner` is printed.
- **Every capture ends on the same line**, logd's `this boot's kernel log is
  /log/<stamp>.log …`. Nothing follows it within the 3 s window, not even
  test-runner's ready marker.
- **The last kernel record is always test-runner's `spawn:`**, at 0.641 to
  0.836 s kernel time. No kernel record reaches the wire after it.
- **At the panic, cpu1 was running `klogd` in every capture**: `cpu1 is on ctx
  0xffff8000002add10 pid=1`, with the panic on cpu0. No green capture is kept to
  compare against, so whether green boots differ here is unknown.

**The reading, not yet tested.** The console's drainer goes quiet after a panic
report taken while it was running on the other CPU. Both kernel records and
program lines then stop reaching the wire. This is the same failure as
`issues/diagnostics/a-cpu-that-stops-passing-takes-the-console-with-it.md`,
reached from a different cause. The other reading is that the recovery itself
stalls a CPU, which the test's flat 3 s window cannot tell apart from a
console that stopped. A capture that stops is not proof the machine stopped.
