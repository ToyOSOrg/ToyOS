---
status: open
kind: defect
opened: 2026-10-24
---

# netstack sends TCP at a third of what the T14's wired link carries

On the T14's I219, a ToyOS TCP sender moves 41 MB/s where the same
program under the bench's Linux moves 113 MB/s on the same path and the
same peer. Received TCP is at parity on that path, so the link, the peer
and the access point are not the limit.

**Measured** on 2026-10-24 with `osbench lan` at `d99ef5a35`. The path is
the T14's I219 at 1000 Mb/s, through the access point, to the AMD bench's
Linux `osbench-peer` on its Wi-Fi. Each cell is one run's p50 of five 3 s
rounds:

| | ToyOS (3 runs) | Linux (4 runs) |
|---|---|---|
| `lan.wired.tcp.upload`, MB/s | 41, 48, 31 | 113, 113, 114, 113 |
| `lan.wired.tcp.download`, MB/s | 112, 112, 109 | 112, 110, 109, 112 |

- No ToyOS round reached a Linux one: ToyOS's rounds ran 31 to 59 MB/s and
  Linux's 112 to 114. The exact rank test over the runs gives p = 1/35.
- The same runs' HTTPS upload to the public internet took 1.40 times
  Linux's time.

Evidence: `ToyOSOrg/evidence` branch `pr-1065`, `r3/`.

The paired session at `aeba59fb0` read 62, 63, 65 and 64 MB/s against
Linux's 113, 110, 111 and 112 (p = 0.029): [evidence `aeba59fb0/`](https://github.com/ToyOSOrg/evidence/tree/37288c84c175a647145fe83fd51d945806cdb4ff/r1/aeba59fb0).

**Not the limit on the 2026-10-27 and -28 heads: the transmit ring's
depth.** At 16 descriptors the ring holds
15 frames, 184 µs of a gigabit wire, and 64 MB/s is exactly 15 segments
every 339 µs, so a ring drained before the pass that refills it was the
candidate. Pull request #1069 read it at 256 descriptors on 2026-10-27,
four ToyOS runs interleaved with four Linux ones: ToyOS 36, 36, 39 and
44 MB/s, Linux 112 in all four. The rounds inside each ToyOS run ranged from
27–35 to 46–67 MB/s, and downloads stayed at parity (110–113). On that
head the deeper ring moved nothing; whether depth limits a sender whose
window grows as it should is not read by it
([evidence `af239c8bb/`](https://github.com/ToyOSOrg/evidence/tree/37288c84c175a647145fe83fd51d945806cdb4ff/r1/af239c8bb)).

**Ruled out: loss.** Pull request #1069's counters read on 2026-10-28, four
runs on the 16-descriptor ring and two on 256, against Linux's 111–112: no
retransmission timeout and no recovery in any run, one 1448-byte segment
resent across all six, no frame missed by the card. Linux resent none. The
256-descriptor ring's two runs read the same 36 MB/s with a ninth to a half
of the 16-descriptor ring's 60,665–66,044 full-ring waits
([evidence `132d5ef75/`](https://github.com/ToyOSOrg/evidence/tree/f06c0e92fa4c7172fe5c0946c540c34b434179dc/r2/132d5ef75)).

**Found on the way: slow start grew once per pass.** [tcp]'s cwnd grew by
one segment for the first ACK after a stop at cwnd, and not for the rest:
netstack hands the stack every ACK one pass received before the pass's
transmit opportunity, so a pass's ACKs grew the window as one. #1069 makes
each ACK count (RFC 5681 §3.1), and the window grow only after an
opportunity cwnd ended, not one the card's room ended (RFC 9438 §5.8).
That is a correction the RFCs require, held by [tcp]'s host tests
`s_cc_016` and `s_cc_017`. Its effect on the T14's upload is read by one
session that alternated it with a control, below; the other readings come
from separate sessions and credit it nothing.

**Read at `afe0c7dac`, 2026-10-28**, a head that also grew the window while
the card held the sender, so neither its rate nor its stop samples are the
per-ACK fix's alone; the corrected head, `d184223b0`, is read below. Upload 58, 59, 58 and 59 MB/s against
Linux's 110–112 ([evidence `afe0c7dac/`](https://github.com/ToyOSOrg/evidence/tree/695218c8513e785bf0465e2f13e0575f5723a2e3/r3/afe0c7dac)). Each ToyOS row's 15 s was
held by cwnd for 1.5–2.1 s, at stops whose mean cwnd was 192–228 KB and
mean smoothed round trip 4.5–5.9 ms; by the peer's window for 87–189 ms;
by the sender itself for about 1 ms. Every resent segment was a loss
probe, and nearly every one came back D-SACKed. Linux ended its rounds at a
mean cwnd of 1,706–2,208 segments and a round trip of 9.7–11.7 ms: its
window is never the limit, and its own queue fills the path.

**Two hypotheses, which the `d184223b0` session tested**:

- The card's refill limits a sender whose window no longer does: at
  `afe0c7dac` the card was full 100,053–106,424 times a row, though that
  head's cwnd times are not creditable. If so, the 256-descriptor ring
  reads faster than 16 at `d184223b0` and its full-ring waits fall.
- The deeper ring read no better on 2026-10-27 and -28 because slow start,
  growing once per pass, held those rows at cwnd. Nothing measured that:
  `af239c8bb` had no counters, and `132d5ef75` charged Nagle holds to
  congestion.

**Read at `d184223b0`, 2026-10-28**
([evidence `d184223b0/`](https://github.com/ToyOSOrg/evidence/tree/bc4188c1070d283e38f7a96019437b99697fe4c5/r4/d184223b0)), four runs on 16 descriptors and two on 256,
against Linux's 112–113:

- On 16 descriptors ToyOS read 58, 61, 59 and 57 MB/s.
- The ring's depth is not the limit. 256 descriptors read 60 and 57 MB/s.
  The card was still full 77,036–79,567 times a row (100,246–106,959 on 16).
  cwnd held the sender 4.6–4.9 s of a row (2.1–2.4 s on 16), at a mean of
  488–506 KB and 8.0 ms (310–354 KB and 4.1–5.6 ms on 16): on the deeper
  ring cwnd was larger and the round trip longer, and the rate was the
  same. That the deeper ring's own queue lengthened the round trip is an
  inference from those three readings, not a measurement.
- No loss: one segment resent, D-SACKed, across the six runs, and no
  probe that ended a slow start.

So the first hypothesis did not hold: with the corrected slow start the
deeper ring still reads no better. The second is not settled by this
session, which has no unfixed control. What remains open
is how fast the card drains what it is given: even at 256 descriptors it
is full about 5,000 times a second. That lead is the I219's transmit
configuration (interrupt moderation, descriptor write-back, how often
netstack learns of completions) and is taken up apart from this record's
pull request.

**The per-ACK fix, measured: 2026-10-28, interleaved with its control**
([evidence `3599f5bb3/`](https://github.com/ToyOSOrg/evidence/tree/d03d7394ed2d34deb1f0c9244bd85f78d7551537/r5/3599f5bb3)). A was `3599f5bb3` with only the per-ACK
fix reverted, every counter kept; B was `3599f5bb3`; four of each, A B A B,
with Linux at the session's start and end (109 and 110 MB/s).

- B read 59, 56, 57 and 57 MB/s; A read 52, 31, 31 and 40. B was faster in
  every pair, and over the eight runs the exact rank test's one-sided p is
  1/70.
- cwnd held each 15 s row 2.2–2.5 s under B, 5.5–8.9 s under A, at a mean
  cwnd of 335–354 KB against 266–301 KB.
- Pooled across the four runs of each, A's rounds spanned 30–60 MB/s and
  B's 55–66.

So in one session the fix raises the rate and narrows the spread of its
rounds, pooled across runs, from 30 MB/s to 11. Heads without it read 31–65 MB/s across
earlier sessions while Linux held 110–114. The fix is consistent with most
of that, but not with `aeba59fb0`'s 62–65, above B's reading here. That
part of the spread across sessions stays unexplained.

**The card sends slowly, read 2026-10-28** ([evidence `c54355277/`](https://github.com/ToyOSOrg/evidence/tree/5db2f7f5ed4e54c12fd74c8aa2368de4b4a6590a/r1/c54355277)).
Pull request #1075 counted, at each full ring, the frames `GPTC` had on the
wire that netstack had not seen written back: none, in either of two runs,
while the ring sat full 10.2–10.4 s of each 15 s row. The part sent what it
held at about 42,000 frames a second, 23.8 µs a frame against 12.3 µs of
wire.

**The cause, derived, which #1075's interleaved reading tests**: the
bring-up wrote `TCTL` whole, clearing `MULR`, which the T14's NVM sets
(`TCTL` read `0x3003f0f8` before the reset). With it clear "the 82574
submits only one request at a time" for transmit data (82574 §10.2.6.1,
§7.2.7). #1075 keeps the fields the part loads.

The transmit setup's own lead stays open in
`issues/the-intel-driver-writes-txdctl-and-tidv-against-the-82574-datasheets-newer-revision.md`.

The clock does not separate the two systems. Each upload row's own
readings, `mhz` just after it and `mhz_settled` before it, were about
1.6 GHz under ToyOS (settled 1.6, 1.8 or 3.8 GHz). Under Linux they were
1.5 GHz (settled 1.5 or 4.2 GHz), and Linux still moved 113 MB/s. At most,
the clock matters through what ToyOS spends per byte sent.

**Exit**: `osbench_lan` on the T14 reads `lan.wired.tcp.upload` within
Linux's run-to-run range on the same path, or the cause is named and owned
by its own issue.
