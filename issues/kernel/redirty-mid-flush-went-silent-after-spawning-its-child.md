---
status: expected-red
kind: defect
opened: 2026-10-01
---

# `redirty_mid_flush` went silent after spawning its child

`642r2-642-whole.log` (`wt/toyos-proclife` `6e9d6a4df`, "ceilings paid at
1.29x", 5882 s for the run): the guest's last words were the child's spawn,
and nothing came after them — not a line from the test, not the kernel's own
ten-second `sched:` lines:

```
[kernel 1.447 cpu1] spawn: /system/bin/test_rs_redirty_mid_flush pid=9 ...
[kernel 1.690 cpu1] spawn: /system/bin/test_rs_redirty_mid_flush pid=10 ...
  STALL redirty_mid_flush  (4484s)
```

The boot is `Profile::Metal` with `test-small-caches`, and `/log` — the file
the two processes race fsyncs on — is the boot stick's, behind xHCI and the
kernel's mass-storage driver. A kernel whose every CPU is inside a call on a
held disk takes no pass and prints nothing
(`issues/kernel/a-held-disk-waits-for-a-pass-no-cpu-takes-when-every-cpu-is-in-a-call-on-it.md`),
and a loaded host is what breaks a stick's transport in its 2000 ms data-phase
budget (`issues/build/smp-ap-hole-and-log-reserve-window-red-under-a-loaded-host.md`);
that is a reading, not a measurement, because the capture holds no kernel line
to confirm it. The test passed in every other whole-suite log kept for this
job, `wt/toyos-proclife` `a8acd2aaa` among them; between that head and
`6e9d6a4df` the branch's own kernel change is two retired-syscall lines, and
the rest is `main`'s merge, which rewrote `drivers/xhci/wait/msc.rs`.

**Exit**: a sighting whose capture names where every CPU was — the next one
needs the console the guard's verdict carries, or a QMP register dump of a
silent guest — and the defect fixed; then the row goes.
