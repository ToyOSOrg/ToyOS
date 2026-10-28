---
status: open
kind: defect
opened: 2026-10-28
---

# [tcp] samples the round trip at a millisecond, and without timestamps once a round trip

With timestamps negotiated, [tcp] takes every RTT sample from the echoed
TSval (`Ts::echo_rtt`, `system/netstack/shard/tcp/src/conn.rs`), and TSval
ticks once a millisecond (`tsval`, `src/lib.rs`). That tick is the finest RFC
7323 §5.4 permits ("1 ms to 1 sec per tick"), so the tick is not what to
change. But a LAN round trip of 1-3 ms reads as 1, 2 or 3 ms.

- On the T14's wired path, srtt read 4-7 ms at cwnd stops and the base round
  trip is about 2 ms. HyStart++'s delay exit (RFC 9406, #1102) compares round
  minima against a threshold of at least 4 ms, so a 1 ms quantum is a quarter
  of it.
- RFC 6298's timer adds G = 1 ms (`rtt.rs`), so RTO is unaffected below its
  200 ms floor.

**Without timestamps, HyStart++'s delay exit is inert.** [tcp] then times one
segment per round trip (Karn's single timer, `timing` in `conn.rs`). A round
never reaches RFC 9406's `N_RTT_SAMPLE` of 8, so the initial slow start grows
with L = 8 until a loss. L = 8 is §4.3's value inside HyStart++, whose delay
exit bounds the overshoot, and §4.3 recommends 8 samples a round. ToyOS offers
timestamps in every SYN, so this needs a peer that declines them.

Nothing has measured an effect. On a 2 ms path a sample can be off by up to
half the round trip.

**Exit**: samples at the stack's clock resolution, one an ACK, with
timestamps and without. One way is RFC 8985 §6.1's per-segment transmission
time, keyed by sequence and read on the ACK that newly acknowledges a segment
never retransmitted. Timestamps would then only disambiguate retransmissions
and drive PAWS. Host-test the sample against a known delay below 1 ms.
