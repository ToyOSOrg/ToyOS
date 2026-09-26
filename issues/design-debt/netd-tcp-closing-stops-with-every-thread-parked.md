---
status: open
kind: defect
opened: 2026-09-26
---

# `netd_tcp_closing` stops with every thread parked

`netd_tcp_closing` opens netd's cap and one more connections one after
another, each written to and dropped against a host peer that never closes.
It stopped in every `cargo test --test toyos-build -- --nightly netd_` on the
dev host, 12 wide under TCG, since `ce0bb621`: there (killed by hand after 21
minutes), at `7687422b` (after 84 minutes) and at `81d0a454` (after 10
minutes), and was green alone each time. It passed in every fast run of the
netd tests the same day, and in the one nightly run before `ce0bb621`, at
`00325ad5`. No stopped run finished the loop.

At `7687422b` the host peer had accepted 65 connections, each `Ok`, and never
a 66th; at `81d0a454`, 5 and never a sixth. The log partition of each stopped
boot's image, read from the host, holds the test's spawn and after it only the
kernel's periodic summaries (at `7687422b`: 594 s, 863 s, 1334 s, 1885 s and
2137 s; at `81d0a454`: 451 s), each saying both CPUs had `ready=0` and
`current=None`, with 4 and 7 threads parked, and 7 pipes still held (of 396
and of 45 allocated). Neither holds a line of netd's after its ready line. So
nothing ran, and netd did not wake for its own deadlines: the streams the
loop had dropped were orphans netd bounds by 60 s or 100 s
(`userland/netd/src/stream.rs`), and a reset past either bound says so in the
log. netd was either parked in a call with no timeout, or in its poller's wait
with a timeout that never returned.

The guest's every wait of its own is bounded (a connect, each `inspect`),
except a netd request's answer, which is what a stopped netd leaves it
waiting on. The harness's 300 s for the case is a budget for one guest:
`qemu::budget` multiplies it by the phase's width and the host's speed, so at
12 wide it is over an hour, and a stop shorter than that is not a timeout.

`blocking_read_window`, the kernel's lost-wake canary, reddened in a wide
fast tier on the same branch the same day
(`issues/kernel/blocking-read-window-missed-its-round-trips-once-in-a-wide-run.md`).
Nothing ties the two yet.

Exit condition: a capture of a stopped boot that names where netd is parked,
or the defect fixed where that capture points.
