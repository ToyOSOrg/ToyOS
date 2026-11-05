---
status: open
kind: finding
opened: 2026-11-06
---

# A userland forward copy a short way above its source modulo 4 KiB stalls on AMD

`sdk/libc`'s x86-64 `memcpy` and `memmove`'s forward branch are one
`rep movsb` (`copy_forward`, `sdk/libc/src/arch/x86_64.rs`). The Software
Optimization Guide for AMD Family 17h Models 30h and Greater Processors
(56305 rev. 3.02, §2.12, p. 39) says the processor "uses address bits 11:0
to determine STLF eligibility" and advises against in-flight stores with the
same bits 11:0 to different addresses where a load may need forwarding from
one of them. A forward copy whose destination is `d` bytes above its source
modulo 4 KiB does exactly that.

On the AMD bench (Ryzen 7 4800U, Zen 2), `osbench copyat` p50s, destination
`d` bytes above the source, main plus measurement only:

| copy | d = 0 | d = 16 |
|---|---|---|
| 256 B | 30 ns | 90 ns |
| 1 KiB | 31 ns | 270 ns |
| 2 KiB | 40 ns | 511 ns |
| 4 KiB | 60 ns | 992 ns |
| 8 KiB | 90 ns | 1953–2615 ns |
| 64 KiB | 0.96 µs | 20.4 µs |
| 1 MiB | 93 µs | 330 µs |
| 64 MiB | 10.9 ms | 21.7 ms |

#1139's in-process rows stall at d = 8, 16, 24 and 31 and at no distance
tried from 32 to 2048, at 4–64 KiB. dlmalloc places two back-to-back 64 KiB
`Vec`s 16 apart, so their 8 KiB copy took 1953–2570 ns against 80–90. The
kernel's own copy goes highest first in that window since #1139.

The T14 (i5-1135G7) read 714–773 ns at d = 16–64 and 206 at 256, against 74
at d = 0, for copyat's ends 1 MiB + d apart. But a heap pair 64 KiB + 16 apart
and #1139's in-process copy at d = 16 did not stall there, so the Tiger Lake
mechanism is not established.

#1133 routed such copies through a descending loop and fixed the stall:
8 KiB at d = 16 in 80 ns, 1 MiB in 21 µs. It was closed because every routed
build measured carried a cost no one could explain. A never-routed 256 KiB
copy at d = 0 ran about 7% slower on `copyat.place.*` with the AVX loop and
with only the SSE2 loop (round 12: 5 of 5 paired boots each), and on
`mem.memcpy.262144` with the AVX loop (0.84–0.86). A base whose `memcpy` was moved to the
head's address did not show it (round 12: 1.03 of base). The SSE2-only loop
also left 8 KiB routed copies at 140 ns against 90 at d = 0.

Evidence: `ToyOSOrg/evidence` branch `pr-1133`:
- the table, from rounds 5 and 8:
  - `782346f21f39f6b2e87329546febaf433a8f7c2f`, `amd-memcpy/round-5-ab/`;
  - `675b23c48eeeeea932715455eca793b4284edb78`, `amd-memcpy/round-8/ab/`;
- the routed builds' cost, from rounds 9–12:
  - `dfd53b1e0d6f481c0812ad62ece2febc1c1d7e0b`, `amd-memcpy/round-9/`;
  - `6d717e2ad8683be5d02947ddc57dd3576e40ada6`, `amd-memcpy/round-10/`;
  - `42d5c114c7826dee22189b9df2e7c04b5b51bebb`, `amd-memcpy/round-11/`;
  - `50787eac11fdb67c517123c7fa096092b4623b74`, `amd-memcpy/round-12/`;
- #1139's rows: `776c503ac402da3dc9c419fb2593a0b8ec79eabe`, `kmemcpy/round-3/`.

**Exit**: on the AMD bench, a libc forward copy at d = 16 within 1.5 times
its d = 0 from 256 B to 1 MiB, with every `mem.memcpy.*` row and the 256 KiB
`copyat.place.*` median within 5% of main's in at least 4 of 5 paired
boots. Alternatively, the routed builds' 256 KiB cost is explained and shown
to be independent of the copy.

**Owner**: the orchestrator, for `sdk/libc/src/arch/x86_64.rs`.
