---
status: open
kind: defect
opened: 2026-10-28
---

# A small memcpy on the AMD bench runs at up to six tenths of Linux's

`osbench mem`'s `mem.memcpy.<bytes>` copies a buffer of that size with each
system's own `memcpy`. On ToyOS that is `sdk/libc`'s, which hands every copy
to the architecture's `copy_forward`; on Linux it is the static musl build's.
The 2026-10-28 scoreboard (ToyOS at 069cbbeb8, 4 boots off the internal NVMe against 5 runs of each bench's Linux as installed, interleaved; p50s, with the run-to-run range; evidence branch `scoreboard-2026-10-28`):

| row | AMD ToyOS | AMD Linux | T14 ToyOS | T14 Linux |
|---|---|---|---|---|
| `mem.memcpy.64` | 3,942 MB/s (0.7%) | 6,492 MB/s (1.7%) | 18,507 MB/s (6.9%) | 6,226 MB/s (1.2%) |
| `mem.memcpy.256` | 14,493 MB/s (0.7%) | 16,158 MB/s (1.7%) | 29,720 MB/s (11.1%) | 24,932 MB/s (1.5%) |
| `mem.memcpy.262144` | 25,696 MB/s (45.1%) | 36,472 MB/s (3.4%) | 55,722 MB/s (0.0%) | 55,634 MB/s (0.1%) |

On the AMD (Ryzen 7 4800U) ToyOS loses 64 bytes at 0.61 of Linux's, 256 at
0.90 and 256 KiB at 0.70, each resolved. On the T14 (i5-1135G7) ToyOS wins
64 bytes threefold and matches at 256 KiB. One copy routine is fast on one
CPU and slow on the other at the same sizes. Its strategy at each size on
Zen 2, a `rep movsb` start-up or otherwise, is not yet read against AMD's
optimization guide. The 256 KiB row's 45% ToyOS range is also unexplained.

**Exit**: `mem.memcpy.*` within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured and
justified against the vendors' guidance.

**Owner**: userland's copy (`sdk/libc/src/memory.rs` and its `arch`).
