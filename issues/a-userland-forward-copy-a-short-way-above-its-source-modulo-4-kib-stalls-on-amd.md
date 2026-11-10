---
status: open
kind: defect
opened: 2026-11-06
---

# A userland forward copy a short way above its source modulo 4 KiB stalls on AMD

`sdk/libc`'s x86-64 `memcpy` and `memmove`'s forward branch are one
`rep movsb` (`copy_forward`, `sdk/libc/src/arch/x86_64.rs`). On the AMD
bench (Ryzen 7 4800U, Zen 2) that copy stalls when its destination is 1 to
31 bytes above its source modulo 4 KiB.

- **Vendor fact.** AMD's publication 56305 rev. 3.02, §2.12, p. 39: the
  processor "uses address bits 11:0 to determine STLF eligibility".
- **Our inference.** In a forward copy `d` bytes above its source modulo
  4 KiB, the load at `src + i` shares those bits with the store just made to
  `dest + i - d`, and waits on it though it needs nothing from it.
- **Measured, not derived.** The stall's window is under 32 bytes. The same
  match exists at larger `d`, and no stall of that size appears there.

`osbench copyat` p50s on that bench, destination `d` bytes above the source,
main plus measurement only:

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

#1139's in-process rows stall at d = 8, 16, 24 and 31, 16 to 34 times
d = 0 at 4–64 KiB, and show no stall of that size at any distance tried
from 32 to 2048. They are not flat there: 64 KiB reads 1553 ns at d = 64 and
96 against 962 at d = 0, and 16 KiB reads 200–210 at d = 32–128 against 150.
dlmalloc places two back-to-back 64 KiB `Vec`s 16 apart, so main's 8 KiB
copy between them took 1953–2570 ns, against 90 at d = 0. The kernel's own
copy goes highest first in that window since #1139.

The T14 (i5-1135G7) read 714–773 ns at d = 16–64 and 206 at 256, against 74
at d = 0, for copyat's ends 1 MiB + d apart. A heap pair 64 KiB + 16 apart
read 121–124 ns there, and #1139's in-process copy at d = 16 read 136–151
against 57–63 at d = 0. So the Tiger Lake window is not established, and
#1139's judgement lists that reading as unexplained.

#1133 routed such copies through a descending loop and fixed the stall:
8 KiB at d = 16 in 80 ns, 1 MiB in 21 µs. It was closed because every routed
build measured carried a cost no one could explain, on never-routed 256 KiB
copies at d = 0:

- **`copyat.place.*`**, a 256 KiB copy between fresh mappings at eight
  placements. Round 12 read the routed build about 7% below base in 5 of 5
  paired boots, with the AVX loop and with only the SSE2 loop. A base whose
  byte-identical `memcpy` was moved to the head's address read 1.03 of base
  in round 12 and 0.98 in round 10. On these rows the cost follows the
  routed code.
- **`mem.memcpy.262144`** is not a clean witness. With the AVX loop it read
  0.84–0.86 of base in rounds 8 to 12. But in round 10 the shifted base
  alone read 0.80 of base in 3 of 3 boots, lower than the head, and in
  round 12 it read base's level. That row already has an unexplained 45%
  run-to-run range on main
  (`issues/a-small-memcpy-on-the-amd-bench-runs-at-as-little-as-six-tenths-of-linuxs.md`).

The SSE2-only loop also left 8 KiB routed copies at 140 ns against 90 at
d = 0.

`osbench copyat` and its `copyat.place.*` rows are not on main. The
measurement-only revision b76abc3bd carries them, reachable from branch
`amd-memcpy-ab8`, which is kept for this record.

Evidence, `ToyOSOrg/evidence`:
- branch `pr-1133`, the table, from rounds 5 and 8:
  - `782346f21f39f6b2e87329546febaf433a8f7c2f`, `amd-memcpy/round-5-ab/`;
  - `675b23c48eeeeea932715455eca793b4284edb78`, `amd-memcpy/round-8/ab/`;
- branch `pr-1133`, the routed builds' cost, from rounds 9–12:
  - `dfd53b1e0d6f481c0812ad62ece2febc1c1d7e0b`, `amd-memcpy/round-9/`;
  - `6d717e2ad8683be5d02947ddc57dd3576e40ada6`, `amd-memcpy/round-10/`;
  - `42d5c114c7826dee22189b9df2e7c04b5b51bebb`, `amd-memcpy/round-11/`;
  - `50787eac11fdb67c517123c7fa096092b4623b74`, `amd-memcpy/round-12/`;
- branch `pr-1139`, #1139's rows:
  `776c503ac402da3dc9c419fb2593a0b8ec79eabe`, `kmemcpy/round-3/`.

**Exit**: on the AMD bench, with b76abc3bd's instrument, a libc forward
copy at d = 16 within 1.5 times its d = 0 from 256 B to 1 MiB, and the
256 KiB `copyat.place.*` median no more than 5% below main's in at least 4
of 5 paired boots. If the routed builds' 256 KiB cost is instead explained
by something other than the route, #1133's route may land, and the stall
bound must still be met. Copies over 1 MiB are left out on purpose: the
descending loop lost to `rep movsb` at 4 MiB in round 5, so the 2x at
64 MiB stays.

**Owner**: the orchestrator, for `sdk/libc/src/arch/x86_64.rs`.
