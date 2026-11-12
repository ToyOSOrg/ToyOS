---
status: open
kind: defect
opened: 2026-11-11
---

# `virt_mask_windows` finds a mask with no hook in a lower-EL data abort

The aarch64 mask-windows kernel panics during `unmap_touch`'s fault, before
any stop begins:

```
panic at kernel/src/windows.rs:79:5: data abort from a lower EL
DOUBLE PANIC: the cpu was already in PageFault
```

A diagnostic build, whose witness logs what it refused before its panic,
named the refusal `cpu0 found interrupts masked with no window open: a mask
reached no hook`, with the machine's stop not begun. So some path from a
lower-EL data abort masks interrupts without the witness's hook.

On main without any branch, guest-rows was red at 422915e2c (run
43027396712) and green at 34cb4c918 (run 43026252506). It was red twice on
an experiment off 422915e2c that changed only the machine's stop; the
diagnostic line is from the second of those runs. It was green once at a
branch off 34cb4c918 (run 43030668596). Logs:
https://github.com/ToyOSOrg/evidence/tree/cf4270976c96690090ad2a3ac8fb4e406e5d49e4
(`guest-main-*.log`, `guest-experiment-*.log`, `guest-rows-*.log`).
It was red again, with the same two panic lines after `unmap_touch`, at
9b7d6d949, #1251's branch on main 36ed75b5a (run 43125192691,
[log](https://github.com/ToyOSOrg/evidence/tree/b554bd2b082f99741c6fdc06a49df74a2a65ab00/r6)).

Before that, every red ran 422915e2c's code and every green later code: 34cb4c918
descends from 422915e2c and adds kernel changes (#1204: the IPC syscalls,
handle objects, the loader's start of a process). So these runs cannot tell
an intermittent defect from one fixed in between. The first step is to repeat
the row at 422915e2c and at current main.

**Exit**: the mask that reaches no hook is found and given one, and
`virt_mask_windows` is green on repeated runs; or repeats at 422915e2c red
and at current main stay green, a change in between is shown to have fixed
it, and this file is deleted.
