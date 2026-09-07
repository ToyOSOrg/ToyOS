---
status: open
kind: finding
opened: 2026-09-07
---

# `latency_wake` reds on the dev host at a rate, and nothing has written that rate down

`cargo run -- --known-red latency_wake` answers `NOT ON THE LIST`, so no
measurement in `src/redlist.rs` has ever named it — which is not a claim that it
is green.

Measured on the dev host (14 cores, macOS, TCG), same session, six runs alone,
three on `metal-suite` + the boot-deadline branch and three on the same tree
stashed back to `metal-suite`:

| arm | p99 | verdict |
|---|---|---|
| deadline | 1606 us | PASS |
| deadline | 1634 us | PASS |
| deadline | 1785 us | PASS |
| base | 1456 us | PASS |
| base | 4096 us (floor, 902 past the histogram) | FAIL |
| base | 1428 us | PASS |

The red is on the **base**, and the arm that touches the timer interrupt entry
did not produce one. The failure mode is always the same: the p99 lands in the
histogram's last bucket, so `4096us` is a floor and the harness refuses it as a
measurement rather than reporting a number it does not have.

So this is a rate on this host, not a classification and not a regression. What
is owed is the rate itself: a `src/redlist.rs` row from enough runs to state
one, or the finding that the histogram's top bucket is too low for a TCG guest
on a loaded laptop.
