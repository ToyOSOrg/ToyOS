---
status: open
kind: defect
opened: 2026-09-08
---

# `loader.log` stops near 16 KiB and says so on a channel nobody keeps

Every T14 boot whose black box carries a wedge record leaves a `loader.log`
that ends in the middle of the sealed record's tail, with none of the lines
the loader writes after it — not the cleared-page check, and not
`Loader log: the last boot is accounted for, so this pass resets the machine`,
which `power::deadline_wedge_chain` and `power::hard_lockup_chain` both
require of the pass after the reset.

Off run 32's readbacks (`wc -c`, `tail -1`):

| boot | bytes | last line |
|---|---:|---|
| `deadlinewedge`, before arm | 16246 | `\| [1.261 cpu7] CPU 7: joining scheduler` |
| `deadlinewedge`, after arm | 16315 | `\| [1.163 cpu3] CPU 3: joining scheduler` |
| `hardlockup`, before arm | 16240 | `\| [1.209 cpu4] sched: cpu=4 ready=0 ...` |
| `jobcase`, before arm | 3216 | the loader's own last line, whole |

Run 27's `deadlinewedge` ended the same way at 16606 bytes, so the cut is near
16 KiB rather than at it. Every short file is one whose report ran long; every
whole one is short.

`loaderlog::line` writes and flushes one line at a time and, on a failed or
short write, takes the file out of the sink and reports through `refused` —
which prints to the firmware's console. So a write that fails part way through
a long report ends the file there, and the one statement of why goes to a
screen no reader of the stick has. Whatever the write failure is, that error
path is why nothing on the stick says the file is incomplete.

Two things are owed: the reason the write stops (the firmware's FAT write, the
file's allocation, or a bound in `RegularFile::write`), and a refusal that
survives — the last thing written to a file that is about to be abandoned
should be that it is being abandoned.

It bites hardest on exactly the boots the black box exists for: a wedged boot's
record is the longest one this loader ever prints.
