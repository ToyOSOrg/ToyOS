---
status: open
kind: defect
opened: 2026-11-01
---

# Userland memcpy on the AMD bench runs at a fifteenth of the T14's

osbench's `ipccopy` (measurement only, branch `ipc-messages-ab-copy`) timed a
`copy_from_slice` of one frame out of a receive buffer into a reused `Vec`, in
the same process and in steady state:

| frame | T14 | AMD |
|---|---|---|
| 4 KiB | 83 ns | 1365 ns |
| 8 KiB | 134 ns | 2645 ns |

That is about 3 GB/s on the AMD bench against about 50 GB/s on the T14. The
kernel's own copies of the same frames on the same boots are faster on the
AMD bench than on the T14: 591 ns against 1074 ns per 8 KiB.

So the slow copy is in userland: the `memcpy` the ToyOS target's std and
compiler-builtins link, or how its pages are mapped there. Which one is not
traced. Every userland copy on that bench pays it. #1121 removed the copy
that made it visible in `ipc.reqrep.*`.

**Exit**: a userland 8 KiB copy on the AMD bench within twice the kernel's
there, measured, with the cause named.
