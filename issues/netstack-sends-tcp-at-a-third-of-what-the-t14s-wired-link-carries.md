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
Linux's 113, 110, 111 and 112 (p = 0.029).

**Refuted: the transmit ring's depth.** At 16 descriptors the ring holds
15 frames, 184 µs of a gigabit wire, and 64 MB/s is exactly 15 segments
every 339 µs, so a ring drained before the pass that refills it was the
candidate. Pull request #1069 read it at 256 descriptors on 2026-10-27,
four ToyOS runs interleaved with four Linux ones: ToyOS 36, 36, 39 and
44 MB/s, Linux 112 in all four. The rounds inside each ToyOS run ranged from
27–35 to 46–67 MB/s, and downloads stayed at parity (110–113). The ring was
not the limit, and a sender that moves either half or all of what it moved
before, round by round, is not limited by any fixed depth.

Still unread, and what the next reading has to separate: retransmissions
and recovery, the congestion window against the peer's window, the time the
sender had nothing it was allowed to send, and the card's full-ring waits.
None of these reaches the log or `inspect` today.

The clock does not separate the two systems. Each upload row's own
readings, `mhz` just after it and `mhz_settled` before it, were about
1.6 GHz under ToyOS (settled 1.6, 1.8 or 3.8 GHz). Under Linux they were
1.5 GHz (settled 1.5 or 4.2 GHz), and Linux still moved 113 MB/s. At most,
the clock matters through what ToyOS spends per byte sent.

**Exit**: `osbench_lan` on the T14 reads `lan.wired.tcp.upload` within
Linux's run-to-run range on the same path, or the cause is named and owned
by its own issue.
