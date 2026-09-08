---
status: open
kind: finding
opened: 2026-09-07
---

# `latency_wake` reds on the dev host at a rate

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

A seventh sighting, in the whole-branch review's twelve-wide `cargo test` on
7294acef: `258 past the 4096us histogram`, and the harness's own isolated re-run
green.

So this is a rate on this host, not a classification and not a regression. The
rate is on the list — two `src/redlist.rs` rows under this name, `FIRES 1 of 6`
alone and one `SEEN` under load, both citing this file. What is still owed is
the other reading of the same evidence: whether cyclictest's 4,096-bucket
histogram is simply too low for a TCG guest on a loaded laptop, which is a
change to the instrument and not to the kernel.
