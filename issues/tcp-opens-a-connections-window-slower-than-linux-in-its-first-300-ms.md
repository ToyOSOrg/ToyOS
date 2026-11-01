---
status: open
kind: defect
opened: 2026-10-24
---

# [tcp] opens a connection's window slower than Linux in its first 300 ms

On the T14's wired path to the AMD bench's Wi-Fi peer, a paced ToyOS TCP
sender (#1114) delivers 6.07 MB in a new connection's first 100 ms and
29.2 MB by 300 ms. In the same session the bench's Linux delivered
6.10–6.38 MB and 28.6–29.6 MB. The upload row reads 114 MB/s against
Linux's 113. Only the 100 ms mark is short, by 30 KB in one boot (the
parity boot, at the end of this record). Before #1114, ToyOS delivered
2.0–3.3 MB and 23.8–25.6 MB. Received TCP is at parity on that path.

This record began as the upload at a third of Linux's (41 MB/s against 113).
What follows is that history, the causes found and removed, and the residual.

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

**Measured 2026-10-28** ([evidence `f9563de4f/`](https://github.com/ToyOSOrg/evidence/tree/d6fbb4b149683056857dd8554c2c77cfbdd465ef/r2/f9563de4f)):
`TCTL`'s `MULR` and `RRTHRESH` set, against the bring-up's whole write
clearing both, raises the upload. One session alternated the two, four of
each, with Linux at both ends (111 and 110 MB/s):

- The bring-up left `TCTL` at `0x3003f0fa` with both fields and `0x0003f0fa`
  without.
- Upload p50 87, 86, 82 and 85 MB/s with them, against 58, 56, 55 and 56
  without: faster in every pair, and over the eight runs the exact rank
  test's one-sided p is 1/70.
- The wire carried 870,000–912,000 frames a row with them, against
  618,000–626,000 without. No completion was unreported at any full answer.
- The ring still sat full 8.3–8.7 s of each 15 s row (10.3–10.6 s without),
  and cwnd held the sender 4.1–4.5 s (2.3–2.6 s without).

**Derived from the datasheet, not separated by the measurement**: which of
the two fields the gain is. 82574 §10.2.6.1 has `MULR` set the number of
read requests for transmit data: "When set to 0b, the 82574 submits only
one request at a time, When set to 1b, the 82574 might submit up to four
concurrent requests". It gives `MULR` an initial value of `1b` and
`RRTHRESH` `01b`.

**The T14's reset does not give §10.2.6.1's initial values.** §10.2.6.1
says `MULR` "is loaded from the NVM word 0x24/0x14", and §4.2 that a
software reset "re-reads the per-function NVM fields" and resets the bits
normally read from the NVM to "their default hardware values". On the T14,
`TCTL` read `0x3003f0f8` both before and after the bring-up's reset
([evidence `19fbf239c/`](https://github.com/ToyOSOrg/evidence/tree/4cbc624ffec885040aa268ad47743ed5df94e51e/r4/19fbf239c)), `CT` and `COLD` included, where those
values would read about `0x30000008`. Either the reset kept the register or
this MAC's own reset value is that one: no reading separates the two, and
the 82574's table does not describe the PCH's MAC. Either way the value the
bring-up met was not one the driver can count on, so it now writes every
`TCTL` field itself, `MULR` and `RRTHRESH` at §10.2.6.1's initial values.

**The rest of the transmit setup the datasheet requires, read 2026-10-28**
([evidence `800e35c69/`](https://github.com/ToyOSOrg/evidence/tree/a911b388eac3990bde439b95091b41a00335748c/r1/800e35c69)): no measured rate effect. Pull request #1078 sets
`TXDCTL` bit 22, which 82574 §10.2.6.11 says "Must be set to 1b for proper
operation", leaves `TIDV` alone, sets `TARC0`'s `ENABLE` and writes the
receive filter registers. One session alternated main's driver (A), that
change (B) and that change with transmit prefetch (C, `PTHRESH` 16,
`HTHRESH` 1), three of each, with Linux at both ends (110 and 111 MB/s):

- Upload p50 A 86, 86 and 85 MB/s, B 85, 86 and 86, C 88, 87 and 87. B reads
  no different from A, and lands as the datasheet's requirement, not a gain.
- C read higher than B in all three rounds, by about 1.5 %, which three runs
  a side cannot resolve: the exact rank test's least p is 1/20 one-sided and
  1/10 two-sided. The prefetch tuning did not land.
- The ring sat full 7.8–8.1 s of each 15 s row in all three arms, and no
  completion was unreported at a full answer.

What is left, about 1.3 times short of Linux with the ring full about half
of each row whatever the descriptor fetch does, is the next lead: what the
part waits on for each frame once it holds it.

**The part keeps line rate while it holds frames; the gap is each
connection's first second, read 2026-10-28** ([evidence `d1ea41856/`](https://github.com/ToyOSOrg/evidence/tree/9f309c28654d89c37e8fb0defc9fd2b1c61cc91b/r1/d1ea41856)).
On the full ring the I219 sent 83,600 frames a second, line rate. One session
read what the peer had received after its first byte, a mean over each row's
five connections, at `d1ea41856` (A) and A with a card-held stop counted as
cwnd's (B), two of each, with Linux at both ends:

| received by | 100 ms | 300 ms | 1 s | 2 s | upload p50 |
|---|---|---|---|---|---|
| Linux (2 runs) | 5.9–6.4 MB | 27.9–29.0 MB | 109–110 MB | 224–225 MB | 113, 113 |
| A (2 runs) | 0.32 MB | 2.0–2.1 MB | 38–41 MB | 149–153 MB | 88, 89 |
| B (2 runs) | 0.32–0.35 MB | 2.1–2.3 MB | 46–47 MB | 162–163 MB | 93, 92 |

- From 1 s to 2 s ToyOS carried 112–117 MB, Linux 115. Nothing was lost.
- The peer acknowledged about 50 KB, 34 segments, an ACK. Each ACK that grew
  cwnd grew it by one SMSS (RFC 5681 §3.1's min(N, SMSS)), so slow start grew
  a few percent a round trip instead of doubling.
- Under A, 75–76 % of ACKs grew nothing: the card's room had ended the
  opportunity before them with more than a segment of cwnd left.

Pull request #1102 replaces the initial slow start with HyStart++
(RFC 9406, which RFC 9438 §4.10 recommends for CUBIC), whose L = 8 for a
sender that does not pace caps the growth an ACK gives at eight segments, and
counts a card-held window as in use when at least half of it is in flight
(RFC 7661 §4.3's validated window, which RFC 9438 §4.6 admits).

**#1102's A/B, 2026-10-28** ([evidence](https://github.com/ToyOSOrg/evidence/tree/8f70147d28011645de1fd458f3091297287b57b9/r2/ab)). A was the fix
reverted, B the fix; three of each, A B A B A B, with Linux at both ends:

| | upload p50, MB/s | received by 100 ms | 300 ms | 1 s | 2 s |
|---|---|---|---|---|---|
| Linux (2 runs) | 113, 113 | 6.4–6.5 MB | 29.7 MB | 110–111 MB | 226–227 MB |
| A (3 runs) | 87, 87, 87 | 0.33–0.34 MB | 2.0–2.1 MB | 34–38 MB | 144–149 MB |
| B (3 runs) | 112, 112, 112 | 2.1–3.3 MB | 23.9–25.4 MB | 106–107 MB | 222–223 MB |

- B was faster in every pair: one-sided p = 1/20. No segment was resent in
  any B run, and no loss probe was sent.
- B's rounds ran 111–115 MB/s. Linux's 113 in this session is above them,
  inside Linux's 109–114 across this record's sessions.
- What is left is the first 300 ms. In every B row HyStart++ entered
  Conservative Slow Start 590–702 times, about 120–140 a connection, and
  resumed every time, never exiting. CSS grows by a quarter while it lasts.
  That this oscillation is the residual is an inference: no run separates it.

The clock does not separate the two systems. Each upload row's own
readings, `mhz` just after it and `mhz_settled` before it, were about
1.6 GHz under ToyOS (settled 1.6, 1.8 or 3.8 GHz). Under Linux they were
1.5 GHz (settled 1.5 or 4.2 GHz), and Linux still moved 113 MB/s. At most,
the clock matters through what ToyOS spends per byte sent.

**#1112's session, 2026-10-29**
([evidence](https://github.com/ToyOSOrg/evidence/tree/96c693b58d242d8ec236e0805b34b714306ec4c2/r1/t14)).
B was #1102's head, C per-segment RTT samples (#1112), D C with L = ∞ and
no pacing, a diagnostic arm that does not land. Three of each, B C D B C D
B C D, with Linux at both ends:

| | upload p50, MB/s | received by 100 ms | 300 ms | 1 s |
|---|---|---|---|---|
| Linux (2 runs) | 113, 114 | 5.9–6.0 MB | 27.2–29.0 MB | 108–110 MB |
| B (3 runs) | 112, 112, 112 | 2.1–2.4 MB | 24.0–24.7 MB | 105.5–105.8 MB |
| C (3 runs) | 112, 112, 112 | 2.2–3.2 MB | 24.3–25.6 MB | 105.2–106.6 MB |
| D (3 runs) | 114, 113, 114 | 5.2–6.4 MB | 28.3–29.7 MB | 109.9–111.2 MB |

- L = 8 is what holds the ramp. Without it D reached Linux's marks, and
  nothing was resent.
- CSS entries were 2.0–2.7 times the rounds that entered CSS, in D as in B
  and C. Most of them are flaps within a round (s_cc_023), and removing them
  was not needed for parity.
- Finer RTT samples (C) did not change the ramp.

RFC 9406 §4.3 gives L = ∞ only to a paced sender. Pull request #1114
paces [tcp] (RFC 9002 §7.7) and removes the cap.

**#1114's A/B, 2026-10-31**
([evidence](https://github.com/ToyOSOrg/evidence/tree/58dcdecab592116996c0e3cc84e4e3c8b7aa50c7/r4/t14)).
C was #1112 after its first review, P #1114's pacing with N = 2 in slow
start, Q P with N = 1.25 throughout. Three of each, C P Q C P Q C P Q, with
Linux at both ends, on trees from before netstack's drivers became their
own processes (#1106):

| | upload p50, MB/s | received by 100 ms | 300 ms | 1 s |
|---|---|---|---|---|
| Linux (2 runs) | 112, 112 | 5.7 MB | 26.1–26.2 MB | 105.7–106.4 MB |
| C (3 runs) | 112, 112, 112 | 2.0–3.1 MB | 23.8–25.4 MB | 105.4–106.8 MB |
| P (3 runs) | 114, 114, 114 | 6.3–6.6 MB | 29.3–29.9 MB | 110.9–111.1 MB |
| Q (3 runs) | 114, 114, 114 | 5.9–6.3 MB | 29.1–29.5 MB | 110.2–111.0 MB |

- P beat C at 300 ms in every pair, and resent nothing.
- P beat Q at 100 ms in every pair, by 0.1–0.5 MB, so N = 2 in slow start
  stays.
- In C every RTT sample but two came from a segment's departure.
- That session's bucket could hold up to a millisecond at the rate, which on
  a short round trip is more than the window. #1114's pacer, as of
  3a0744d34, holds at most half the window. The parity read waits on a boot
  of that head, on current main.

**The parity boot, 2026-11-01**
([evidence](https://github.com/ToyOSOrg/evidence/tree/8f08a8e9399817fc669e395dc717ee236a18ed35/r7/t14)).
One boot of #1114 at 3a0744d34, on main after #1106, with Linux before and
after it:

| | upload p50, MB/s | received by 100 ms | 300 ms | 1 s | 2 s |
|---|---|---|---|---|---|
| Linux (2 runs) | 113, 113 | 6.10, 6.38 MB | 28.63, 29.56 MB | 109.1, 110.5 MB | 224.6, 226.1 MB |
| ToyOS | 114 | 6.07 MB | 29.19 MB | 110.7 MB | 227.0 MB |

- The rule set before the run asked for the 100 ms and 300 ms marks at or
  above the lower Linux run's, p50 within 1 MB/s, and nothing resent. The
  300 ms mark, p50 and loss passed. The 100 ms mark missed by 30 KB, 0.5 %,
  so the rule is not met and this record stays open.
- That gap is smaller than the spread of the marks across runs: Linux's two
  runs differ by 280 KB, and the A/B's three P runs by 260 KB. One boot does
  not separate it from noise.
- Nothing was resent, and no RTO, recovery or loss probe occurred. Every RTT
  sample came from a departure (33,319, none from the echo). CSS entered in
  134 rounds, 420 times, and never exited.
- The pacer held 73 ms over the row, against 33–41 ms with the A/B's
  bucket. The half-window bound holds more on the short round trip, as
  expected.

**The 100 ms miss is not resolved by the bench, 2026-11-01.** Three readings
say the parity boot's 30 KB is below what the session form can tell apart:

- The rule cannot pass reliably at parity. With one ToyOS run between two Linux
  runs and all three drawn alike, ToyOS is the lowest of the three one time in
  three. A third of sessions would miss at exact parity.
- The instrument's step is a read. The peer counts at a mark what it had
  before the read that passed it, and it reads up to 64 KiB at a time
  (`osbench-lan`'s `RampCount`). So each round's mark can sit up to 64 KiB
  low, and how far depends on how the sender's segments meet the peer's
  reads.
- The marks spread more than the miss, and where they spread is the session.
  Linux's two runs differed by 280 KB at 100 ms in the parity boot. Across
  this record's sessions Linux read 5.7–6.5 MB. The paced sender read 6.3–6.6
  MB in its A/B, above both Linux runs (5.7 MB) in all three pairs, and 6.07 MB
  in the parity boot.

A model of the ramp gives no candidate either. On [tcp]'s host network, a
1 Gb/s sender into a card at 78 frames a millisecond (113 MB/s) at round trips
of 2, 4 and 8 ms delivers 10.26, 8.27 and 3.49 MB by 100 ms.

- Two changes move those by under 0.1 MB: the 1 ms bucket without the
  half-window bound, and a window stop that grows cwnd at half of cwnd in
  flight, as a card stop does (RFC 9438 §4.6 admits RFC 7661 there).
- Removing pacing costs 0.07, 0.48 and 1.1 MB.

That model's receiver is [tcp]'s, not the peer's Linux. On the bench, both
senders are held by the peer's window first: ToyOS for 66 ms a connection in
the parity boot, Linux for 92–148 ms. So the first 100 ms reads mostly the
receiver's window growth, which both senders share.

`osbench` now reports each round's marks beside their mean
(`peer_received_by_<ms>ms_bytes`), so a session gives five connections a
run and the run's own spread.

**The exit's first session read no ToyOS row, 2026-11-01**
([evidence](https://github.com/ToyOSOrg/evidence/tree/35f09268e09714a8547496bdd766bcd3ca998325/r1)). This was Linux, ToyOS at `4f52c3056`, Linux, all
exiting 0. ToyOS's `osbench_lan` found the peer and then refused the group:
`no netstack link holds 10.0.2.15`. std's `TcpStream::local_addr` answers
QEMU's user-network address whatever the lease is, and since #1122 osbench
names a connection's link by that address. The T14's lease is not it. So the
session does not decide. The next one runs on a head where netstack's
answers carry each stream's local address and std, libc and the SDK return it.

Its Linux runs do give the per-connection spread the rule needs:

- p50 113 and 113 MB/s.
- At 100 ms the rounds ran 5.82–6.63 MB and 5.13–6.38 MB, a standard
  deviation of 478 KB over the ten. The parity boot's 30 KB miss is a
  sixteenth of that.
- At 300 ms they ran 28.8–30.0 MB and 27.7–29.7 MB.
- The two runs do not differ: the two-sided rank test gives p = 0.69 at
  100 ms and 0.42 at 300 ms. So the rule's exchangeability check passes.

**Exit**: in one T14 session, Linux, ToyOS, Linux, `osbench_lan`'s
`lan.wired.tcp.upload` reads:

- p50 at or above the lower Linux run's, less 1 MB/s;
- nothing resent beyond a loss probe;
- at 100 ms and at 300 ms, ToyOS's five rounds not ranked below Linux's ten:
  the exact one-sided rank test gives p > 0.05, so parity fails one session
  in twenty.

If Linux's two runs differ at either mark at p ≤ 0.05 by the exact two-sided
rank test, the connections are not exchangeable across runs. Then the session
does not decide, and an interleaving of Linux, ToyOS, Linux, ToyOS, Linux
compares runs instead. Or the cause is named and owned by its own issue.
