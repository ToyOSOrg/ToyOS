---
status: open
kind: defect
opened: 2026-10-04
---

# Every loader line costs the T14 a fixed 13 ms, whatever its length

The T14's `loader.log` from a `jobcase` boot at `adb0a86c3` (each line stamped
with its ms since the loader's entry, at the 2419200000 Hz CPUID 15H states,
and the stamps agreeing with the kernel's HPET-calibrated `loader 1877 ms`)
spaces consecutive lines 12-14 ms apart wherever the loader does no other work
between them, and the length of the line does not move it: `Kernel: 3225456
bytes` (21 characters) follows its predecessor by 12 ms, each ~150-character
`Loading segment:` line by 13-14 ms.

Each line is two writes: ConOut's `OutputString`, and `loaderlog::line`'s
`file.write` and `file.flush()` to `loader.log` on the stick's FAT
(`bootloader/src/loaderlog.rs`). The file is the cost. The boot pass's
`Loader lines:` line from the T14's `jobcase` boot at `d373045ee` reads: the
43 lines above it took 129796443 counter ticks on the console (53.7 ms, 1.2 ms
a line) and 1505476678 in `loader.log` (622.3 ms, 14.5 ms a line), and the
screen's clear before the first line 250073763 (103.4 ms), at 2419200000 Hz.

The fix is fewer flushes, and that gives up `loaderlog`'s claim that a line is
written and flushed before the loader goes on: what a hang between two flushes
would lose is a decision of its own, not a side effect of the fix.

Exit: the T14's loader lines cost what their content costs to draw and keep,
with the durability `loader.log` keeps stated at `loaderlog`.
