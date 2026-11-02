---
status: open
kind: defect
opened: 2026-11-02
---

# A user copy a short way above its source modulo 4 KiB still costs

`sdk/libc`'s `copy_forward` (`sdk/libc/src/arch/x86_64.rs`) copies highest
first when it has at least 64 bytes and at most 1 MiB, its destination is
1 to 511 bytes above its source modulo 4096, and the two ranges are disjoint.
Every other copy is one `rep movsb`. Each load of a forward copy at such a
distance matches an older, unrelated store on address bits 11:0 (AMD
56305 rev. 3.02, §2.12). Before this route the copy was 10 to 29 times
slower. Round 5 of `osbench copyat` measured what is left. That was base
80421e74a against head 0f861df89, four boots per bench, p50s, with the
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

The `mem.memcpy.64` outlier is reproduced and explained, or shown to be
independent of this copy.

**Owner**: the orchestrator, for `sdk/libc/src/arch/x86_64.rs`.
