---
status: open
kind: defect
opened: 2026-10-28
---

# A small memcpy on the AMD bench runs at as little as six tenths of Linux's

`osbench mem`'s `mem.memcpy.<bytes>` copies a buffer of that size with each
system's own `memcpy`. On ToyOS that is `sdk/libc`'s, whose `copy_forward` is
one `rep movsb` at every length (`sdk/libc/src/arch/x86_64.rs`). On Linux it
is the static musl build's.

From the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`):
ToyOS at 069cbbeb8, four boots off the internal NVMe, against five runs of
each bench's Linux as installed, interleaved. A cell is the p50 with its
run-to-run range.

| row | AMD ToyOS | AMD Linux | T14 ToyOS | T14 Linux |
|---|---|---|---|---|
| `mem.memcpy.64` | 3,942 MB/s (0.7%) | 6,492 MB/s (1.7%) | 18,507 MB/s (6.9%) | 6,226 MB/s (1.2%) |
| `mem.memcpy.256` | 14,493 MB/s (0.7%) | 16,158 MB/s (1.7%) | 29,720 MB/s (11.1%) | 24,932 MB/s (1.5%) |
| `mem.memcpy.16384` | 75,330 MB/s (1.2%) | 73,707 MB/s (3.0%) | 185,364 MB/s (0.3%) | 201,387 MB/s (17.9%) |
| `mem.memcpy.262144` | 25,696 MB/s (45.1%) | 36,472 MB/s (3.4%) | 55,722 MB/s (0.0%) | 55,634 MB/s (0.1%) |
| `mem.memcpy.67108864` | 6,140 MB/s (0.5%) | 6,302 MB/s (4.5%) | 9,257 MB/s (0.6%) | 9,677 MB/s (1.2%) |

On the AMD (Ryzen 7 4800U) ToyOS loses 64 bytes at 0.61 of Linux's, 256 at
0.90 and 256 KiB at 0.70, each resolved. Zen 2 lacks ERMS
(`issues/kernel-forward-copies-and-fills-are-one-rep-movsb-or-stosb-on-every-cpu.md`),
and a `rep movsb` start-up is the cost
`issues/a-short-memset-on-x86-64-costs-a-rep-stosb-start-up.md` records for
`memset`. This row is that record's `memcpy` measurement. On the T14
(i5-1135G7) ToyOS wins 64 bytes threefold. It loses 16 KiB (1.09x) and
64 MiB (1.05x), both resolved, and matches at 256 KiB. The sweep asks for
huge pages, which Linux gives and ToyOS does not; the 64 MiB row's 0.95 of
Linux's equals `mem.bw.copy`'s under the same difference
(`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`). The 256 KiB row's 45%
ToyOS range on the AMD is unexplained.

**Exit**: `mem.memcpy.*` within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured and
justified against the vendors' guidance.

**Owner**: userland's copy (`sdk/libc/src/memory.rs` and its `arch`).
