---
status: open
kind: defect
opened: 2026-09-29
---

# Most T14 leases land one DHCP retry late

Over the LAN boots in the readbacks of three T14 runs, the I219's link came up
2.72–2.82 s after its driver every time. Of the 16 leases, 5 landed 3.2–3.6 s
after netd came up and 11 at 13.1–13.4 s, so the first lease of a boot, and
with it `sshd`, arrived at 9.1–9.3 s of boot on 4 of the 13 boots and at
19.0–19.1 s on the other 9.

The ten-second step is smoltcp 0.12's default `RetryConfig::discover_timeout`,
which netd keeps. netd restarts
discovery when the link comes up (`userland/netd/src/dhcp.rs:42-61`), and on
the late boots the lease came from the DISCOVER sent one timeout after that
one. What became of the first, whether it left the machine and whether an
OFFER came back, is unmeasured. RFC 2131 §4.1 puts the first retransmission
at 4 s, randomized by ±1 s.

The network track owns it: netd leaves smoltcp in stage 5 of
`issues/toyos-has-its-own-network-stack.md`, whose stage 3 is
`toyos-dhcp`. Stage 6 of
`issues/the-loader-does-only-what-must-precede-the-handover.md`
waits on it.

netstack runs `toyos-dhcp` now (`userland/netstack/node`): an unanswered
DISCOVER is sent again on RFC 2131 §4.1's schedule, and a link that comes up
with no lease starts the exchange over
(`a_link_that_returns_with_no_lease_starts_discovery_over_at_once`,
`userland/netstack/node/tests/lease.rs`). Every reading above is of the
smoltcp client; none has been taken on the T14 since.

**Exit**: netd logs each DISCOVER it sends and each OFFER it receives, and a
T14 boot's log shows what became of the DISCOVER sent as the link came up; on
every boot of a T14 run an unanswered DISCOVER is sent again within RFC 2131
§4.1's 4 ± 1 s.
