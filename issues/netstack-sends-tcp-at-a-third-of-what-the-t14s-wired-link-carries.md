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

**Refuted: the transmit ring's depth.** At 16 descriptors the ring holds
15 frames, 184 µs of a gigabit wire, and 64 MB/s is exactly 15 segments
every 339 µs, so a ring drained before the pass that refills it was the
candidate. Pull request #1069 read it at 256 descriptors on 2026-10-27,
four ToyOS runs interleaved with four Linux ones: ToyOS 36, 36, 39 and
44 MB/s, Linux 112 in all four. The rounds inside each ToyOS run ranged from
27–35 to 46–67 MB/s, and downloads stayed at parity (110–113). The ring was
not the limit, and a sender that moves either half or all of what it moved
before, round by round, is not limited by any fixed depth
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

**Read at `afe0c7dac`, 2026-10-28**, a head that also grew the window while
the card held the sender, so neither its rate nor its stop samples are the
per-ACK fix's alone; the corrected head's reading is pending. Upload 58, 59, 58 and 59 MB/s against
Linux's 110–112 ([evidence `afe0c7dac/`](https://github.com/ToyOSOrg/evidence/tree/695218c8513e785bf0465e2f13e0575f5723a2e3/r3/afe0c7dac)). Each ToyOS row's 15 s was
held by cwnd for 1.5–2.1 s, at stops whose mean cwnd was 192–228 KB and
mean smoothed round trip 4.5–5.9 ms; by the peer's window for 87–189 ms;
by the sender itself for about 1 ms. Every resent segment was a loss
probe, and nearly every one came back D-SACKed. Linux ended its rounds at a
mean cwnd of 1,706–2,208 segments and a round trip of 9.7–11.7 ms: its
window is never the limit, and its own queue fills the path.

So at that head neither loss, the peer, the sender nor cwnd held most of a
ToyOS row. What is left is the card: 100,053–106,424 full-ring waits a row, about
6,700 a second. The 256-descriptor ring read no better on 2026-10-27 and
-28 because slow start then held every row at cwnd; with that fixed, the
ring's depth is again the candidate, which a reading of both rings at the
corrected head settles. The transmit setup's own lead stays open in
`issues/the-intel-driver-writes-txdctl-and-tidv-against-the-82574-datasheets-newer-revision.md`.

The clock does not separate the two systems. Each upload row's own
readings, `mhz` just after it and `mhz_settled` before it, were about
1.6 GHz under ToyOS (settled 1.6, 1.8 or 3.8 GHz). Under Linux they were
1.5 GHz (settled 1.5 or 4.2 GHz), and Linux still moved 113 MB/s. At most,
the clock matters through what ToyOS spends per byte sent.

**Exit**: `osbench_lan` on the T14 reads `lan.wired.tcp.upload` within
Linux's run-to-run range on the same path, or the cause is named and owned
by its own issue.
