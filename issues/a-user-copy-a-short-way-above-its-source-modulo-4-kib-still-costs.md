---
status: open
kind: defect
opened: 2026-11-02
---

# A user copy a short way above its source modulo 4 KiB still costs

`sdk/libc`'s `copy_forward` (`sdk/libc/src/arch/x86_64.rs`) copies highest
first when it has at least 256 bytes and at most 1 MiB, its destination is
1 to 511 bytes above its source modulo 4096, and the two ranges are disjoint.
Every other copy is one `rep movsb`. A copy under 256 bytes never asks the
distance, and the check is inline for the rest: round 7 measured every 64 B
to 1 KiB copy 5-7% slower on the AMD when every copy made a call to ask.

The two benches stall over different windows, and the route covers both:
- **AMD (Ryzen 7 4800U, Zen 2).** The processor judges store-to-load
  forwarding eligibility by address bits 11:0 (Software Optimization Guide
  for AMD Family 17h Models 30h and Greater Processors, 56305 rev. 3.02,
  §2.12). A forward `rep movsb` from 4 KiB to 64 KiB stalls at d = 8, 16,
  24 and 31 and at no distance tried from 32 to 2048 (#1139's in-process
  `kcopyat.movsb` rows, `ToyOSOrg/evidence` commit
  `776c503ac402da3dc9c419fb2593a0b8ec79eabe`, `kmemcpy/round-3/`). Up to
  64 MiB it stalls at d = 16 and not at 256 (`copyat`, round 5). At 8 KiB
  and d = 16 it takes 2615 ns against 90 at d = 0.
- **T14 (i5-1135G7, Tiger Lake).** `copyat`'s copies, whose ends are 1 MiB
  plus the distance apart, read 714–773 ns at d = 16–64 and 206 at 256
  against 74 at d = 0, then 122 at 512, 108 at 640 and 88 at 1024. The
  cost tapers rather than ending, and 512 is a choice within the taper.
  Intel documents 4K aliasing on bits 11:0, but the same bench read 124 ns
  for two heap `Vec`s 64 KiB + 16 apart and 136 for #1139's in-process copy
  at d = 16, so what decides the T14's stall is not established.

Round 5 of `osbench copyat` measured what is left after the route. That was
base 80421e74a against head 0f861df89, four boots per bench, p50s, with the
destination `d` bytes above the source:

- **T14 (i5-1135G7), up to 1 MiB.** Routed 8 KiB copies take 82–103 ns
  against 59 at d = 0, which is 1.4–1.7 times as long; base read 714–773 ns
  at d = 16–64. The worst rows (s0.d16 at 101, s16.d32 at 103) are where the
  loop's aligned 32-byte stores leave its loads 16 bytes off a 32-byte
  boundary, so every other load is split. The two heap `Vec`s dlmalloc places
  16 apart copy in 92 ns, 1.56 times d = 0.
- **AMD (Ryzen 7 4800U), over 1 MiB.** Those copies keep `rep movsb`, which
  stalls as before: 4 MiB at d = 16 takes 1.32 ms against 386 µs at d = 0
  (3.4 times), and 64 MiB at d = 16 takes 21.7 ms against 10.9 (2.0 times).
  The descending loop beat it at d = 16 there, 612 µs and 9.8 ms. At 4 MiB and
  d = 256, a distance that does not stall, it lost: 597 µs against 385.

- **AMD, below 4 KiB.** Round 8 (base a049276a6, two boots, p50s within
  10%) read, at d = 0 and d = 16: 256 B 30 and 90 ns, 1 KiB 31 and 270,
  2 KiB 40 and 511 (4 KiB: 60 and 992). Copies from 256 B are routed
  (`ROUTED_LEAST`), which C6 judges. Nothing below 256 B is routed, and no
  copy under 256 B has been timed at a stalling distance.

- **AMD, `mem.memcpy.262144` follows code placement.** Rounds 7-9 read the
  head at 0.906, 0.84 and 0.858 of base on that row, which never takes the
  route. In round 10, a base whose `memcpy` was byte-identical but moved to
  the head's address (by padding in `memcmp`, 4f14822ce) read 32030-35364
  MB/s, 0.80 of base's 38395-40764, against the head's 34397-35068. The rule
  written for that round asked the shifted base to land inside the head's
  range and failed because it landed below it. The orchestrator read that
  by the rule's purpose, not its literal range clause: the move alone
  reproduces the drop, so the row is sensitive to where `memcpy` sits, and
  D3 passes on it. Which placement property matters is not established.
  Evidence: `ToyOSOrg/evidence` commit
  `6d717e2ad8683be5d02947ddc57dd3576e40ada6`, `amd-memcpy/round-10/`.

One T14 head boot read `mem.memcpy.64` at 6873 MB/s in all 15 rounds. The
other three boots read 16649–18226. That copy is page-aligned at d = 0 and
never takes the route. It is unexplained.

Evidence: `ToyOSOrg/evidence` branch `pr-1133`, commit
`782346f21f39f6b2e87329546febaf433a8f7c2f`, `amd-memcpy/round-5-ab/`
(`judgement.md`, `table.txt` and every boot's rows) and
`amd-memcpy/round-5-ab-rule.txt`.

**Exit**: round 5's rules as written, measured on both benches.
- C1: libc 8192 rows at d = 16..384, from both source offsets, within 1.3
  times libc.s0.d0, with 10 ns allowed on the AMD.
- C3: `copyat.8192.heap` within 1.5 times libc.s0.d0.
- C4: `copyat.<size>.libc.s0.d16` and `.d256`, for every size from 64 KiB to
  64 MiB, within 1.1 times base's same row, and on the AMD also within 1.1
  times base's d = 0 row of that size.

- C6: `copyat.<size>.libc.s0.d16` for 256 B, 1 KiB and 2 KiB within 1.5
  times that size's d = 0 on the AMD, with `mem.memcpy.256`, `.1024` and
  `.4096`, the rows whose copies are short and ask the distance, within 1.1
  times main's; or a recorded decision that short copies keep the stall
  because routing them costs more than it saves.

The `mem.memcpy.64` outlier is reproduced and explained, or shown to be
independent of this copy.

**Owner**: the orchestrator, for `sdk/libc/src/arch/x86_64.rs`.
