---
status: open
kind: defect
opened: 2026-10-28
---

# A small memcpy on the AMD bench runs at as little as six tenths of Linux's

`osbench mem`'s `mem.memcpy.<bytes>` copies a buffer of that size with each
system's own `memcpy`. On ToyOS that is `sdk/libc`'s (`sdk/libc/src/arch/x86_64.rs`):
one `rep movsb` at every length on a CPU that declares fast short `rep movsb`
(FSRM), such as the T14's; elsewhere, such as on the AMD bench, a copy of up
to 256 bytes is moves from each end and a longer one `rep movsb`. Until
2026-11 it was one `rep movsb` everywhere, which the tables below measure. On
Linux it is the static musl build's.

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

**Current state, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339)): ToyOS at main ad5db5a65, four
boots a bench interleaved with five runs of its Linux as installed; a cell is
the median of the runs' p50s with their range, and the last column of each
bench is ToyOS against Linux, above 1 slower. The AMD bench's Linux timed
runs 1 to 3 through the HPET, so its medians overstate Linux's per-operation
times there.

| row | unit | T14 ToyOS | T14 Linux | T14 | AMD ToyOS | AMD Linux | AMD |
|---|---|---|---|---|---|---|---|
| `mem.bw.copy` | MB/s | 6,652 (1.6%) | 6,951 (3.2%) | 1.04x | 8,304 (1.6%) | 8,300 (5.9%) | 1.00x |
| `mem.memcpy.1024` | MB/s | 82,894 (4.0%) | 70,434 (21.0%) | 0.85x | 44,282 (0.7%) | 40,936 (1.6%) | 0.92x |
| `mem.memcpy.1048576` | MB/s | 25,585 (6.2%) | 25,552 (9.7%) | 1.00x | 11,528 (2.6%) | 11,575 (2.7%) | 1.00x |
| `mem.memcpy.16384` | MB/s | 185,546 (2.6%) | 197,542 (18.3%) | 1.06x | 88,790 (0.7%) | 73,843 (3.5%) | 0.83x |
| `mem.memcpy.16777216` | MB/s | 10,423 (3.2%) | 10,760 (3.2%) | 1.03x | 6,162 (0.4%) | 6,162 (5.6%) | 1.00x |
| `mem.memcpy.256` | MB/s | 28,580 (5.2%) | 25,033 (1.1%) | 0.88x | 14,598 (0.9%) | 16,150 (1.7%) | 1.11x |
| `mem.memcpy.262144` | MB/s | 55,741 (0.0%) | 55,594 (0.1%) | 1.00x | 38,081 (16.9%) | 35,246 (5.2%) | 0.93x |
| `mem.memcpy.4096` | MB/s | 165,024 (0.0%) | 158,167 (4.9%) | 0.96x | 89,524 (0.6%) | 84,949 (1.9%) | 0.95x |
| `mem.memcpy.4194304` | MB/s | 20,371 (1.3%) | 21,445 (7.8%) | 1.05x | 10,916 (2.1%) | 10,745 (1.6%) | 0.98x |
| `mem.memcpy.64` | MB/s | 18,864 (6.7%) | 6,262 (0.5%) | 0.33x | 3,967 (0.8%) | 6,492 (1.6%) | 1.64x |
| `mem.memcpy.65536` | MB/s | 55,434 (0.0%) | 55,001 (0.4%) | 0.99x | 67,126 (0.7%) | 65,907 (1.7%) | 0.98x |
| `mem.memcpy.67108864` | MB/s | 9,308 (1.4%) | 9,647 (0.7%) | 1.04x | 6,212 (1.2%) | 6,145 (2.5%) | 0.99x |

On the AMD bench 64 bytes (0.61 of Linux's) and 256 bytes (0.90) are still
behind; 256 KiB is unresolved, ToyOS's boots reading 35.2 to 41.6 GB/s. On
the T14 4 MiB, 16 MiB, 64 MiB and `mem.bw.copy` are behind by 3 to 5%.

**Candidates, 2026-11-11** ([evidence](https://github.com/ToyOSOrg/evidence/tree/cf6a882c04a646a8998e3bf0ad66e9f699b8e8aa/memperf/diagnosis)):
six copies timed as the sweep times them, one boot and one Linux run a
bench. Linux's `memcpy` there is musl's, `rep movsq` between byte loops. On
the AMD bench a 64-byte copy took 15.8 ns by `rep movsb`, 8.7 by
`rep movsq`, 5.4 by a 16-byte vector loop and 5.0 by a 32-byte one; at 256
bytes 17.1, 14.3, 16.8 and 10.0; from 1 KiB `rep movsb` was fastest. On the
T14 `rep movsb` was fastest or within half a nanosecond of it at every size
(3.3 ns at 64 bytes against 5.0 by the 16-byte loop), so a vector path there
would cost 1.5 to 2 times at 64 to 256 bytes. No one routine wins on both.
On the T14 `mem.bw.copy`, 16 MiB and 64 MiB copies trail Linux by 3.7 to
4.9% over its 4 KiB pages and its huge ones alike; a 4 MiB copy read 1.0%
under Linux's over 4 KiB pages and 10.0% under its over huge ones
(`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`).

- **Owed on metal** for the choice by FSRM: an A/B on both benches, main
  from before and after it, of `osbench_mem` and of `osbench copyat`.
  `copyat` is not on main: its rows are the measurement-only commit carried
  by branches `memperf-copy-ab-base` and `memperf-copy-ab-head3`, #1133's
  rows as one commit.
  - On the AMD bench, which lacks FSRM, main after must read
    `mem.memcpy.64` and `mem.memcpy.256` faster.
  - On the T14, which has it, no `mem.memcpy.*` row may be slower.
  - On both, neither `mem.memcpy.262144` nor the eight `copyat.place.*` rows
    may be slower. The eight are judged together, by each boot's mean. #1133
    was closed for a drop there that followed code placement.
  - An AMD result whose boots spread by more than 5% is inconclusive and
    asks for more rotated blocks.
  - Until then the gain is unmeasured.

**Exit**: `mem.memcpy.*` within Linux's on both benches in a labelled
current-state comparison, or each remaining difference measured and
justified against the vendors' guidance.

**Owner**: userland's copy (`sdk/libc/src/memory.rs` and its `arch`).
