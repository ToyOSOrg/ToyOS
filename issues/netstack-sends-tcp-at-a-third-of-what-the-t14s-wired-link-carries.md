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

**Cause, derived and not yet read on the T14**: the I219's transmit ring held
16 descriptors, so at most 15 frames were the part's at once: 184 µs of a
gigabit wire. A pass that finds the ring full sleeps until the part writes a
descriptor back, and its interrupt is held back by the 131 µs throttle
(`THROTTLE`, `system/netstack/i219/src/lib.rs`) before netstack is even woken.
64 MB/s is 15 full-sized segments every 339 µs. The ring is now 256
descriptors, 3.1 ms of the wire.

The clock does not separate the two systems. Each upload row's own
readings, `mhz` just after it and `mhz_settled` before it, were about
1.6 GHz under ToyOS (settled 1.6, 1.8 or 3.8 GHz). Under Linux they were
1.5 GHz (settled 1.5 or 4.2 GHz), and Linux still moved 113 MB/s. At most,
the clock matters through what ToyOS spends per byte sent.

**Exit**: `osbench_lan` on the T14 reads `lan.wired.tcp.upload` within
Linux's run-to-run range on the same path, or the cause is named and owned
by its own issue. The next reading is pull request #PR's.
