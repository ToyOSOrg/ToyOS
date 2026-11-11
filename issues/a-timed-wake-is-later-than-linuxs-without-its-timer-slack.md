---
status: open
kind: defect
opened: 2026-11-11
---

# A timed wake is later than Linux's without its timer slack

`osbench`'s timer rows record how late a timed wake runs:
`time.sleep_late.*` after a `thread::sleep` of each length,
`wake.periodic.idle` and `time.timer_1ms.late` after a sleep to absolute
targets every period, and `wake.periodic.loaded` the same with a spinner on
every CPU (`tests/guest/osbench/src/time.rs`, `wake.rs`). Linux pads an
ordinary sleep by the thread's timer slack, 50 µs by default; ToyOS has
none. So each row has a Linux-only `.slack1ns` variant with the slack at
1 ns, and that is the Linux a ToyOS timer is like for like with.

From the 2026-11-11 scoreboard ([evidence](https://github.com/ToyOSOrg/evidence/tree/784d15e532d89aaa178162a1546024af1fc851df), `slack.md` per bench): ToyOS at main ad5db5a65, four
boots a bench interleaved with five runs of its Linux as installed. A cell
is the median of the runs' p50s, ns late; p is the exact two-sided
Mann-Whitney test over the runs. The session's rule was set against the
default rows, so the slack-free reading is exploratory.

**The T14:**

| row | ToyOS ns | Linux, default slack | ToyOS against it | Linux, slack 1 ns | ToyOS against it | p | reading |
|---|---|---|---|---|---|---|---|
| `time.sleep_late.100us` | 5,210 | 51,850 | 0.10x | 1,827 | 2.85x | 0.016 | behind |
| `time.sleep_late.10ms` | 31,318 | 218,198 | 0.14x | 163,553 | 0.19x | 0.016 | ahead |
| `time.sleep_late.10us` | 1,434 | 51,819 | 0.03x | 1,820 | 0.79x | 0.016 | ahead |
| `time.sleep_late.1ms` | 25,639 | 72,340 | 0.35x | 20,400 | 1.26x | 0.286 | unresolved |
| `time.sleep_late.1us` | 1,501 | 51,814 | 0.03x | 3,982 | 0.38x | 0.016 | ahead |
| `time.timer_1ms.late` | 14,155 | 70,302 | 0.20x | 20,087 | 0.70x | 0.190 | unresolved |
| `wake.periodic.idle` | 20,806 | 67,862 | 0.31x | 17,667 | 1.18x | 0.016 | behind |
| `wake.periodic.loaded` | 3,624 | 52,374 | 0.07x | 2,373 | 1.53x | 0.016 | behind |

**The AMD bench**, whose Linux timed runs 1 to 3 through the HPET:

| row | ToyOS ns | Linux, default slack | ToyOS against it | Linux, slack 1 ns | ToyOS against it | p | reading | slack-free TSC runs |
|---|---|---|---|---|---|---|---|---|
| `time.sleep_late.100us` | 5,688 | 65,595 | 0.09x | 11,189 | 0.51x | 0.016 | ahead | 10,351, 10,433 |
| `time.sleep_late.10ms` | 56,524 | 118,624 | 0.48x | 289,050 | 0.20x | 0.016 | ahead | 289,050, 297,859 |
| `time.sleep_late.10us` | 1,367 | 61,099 | 0.02x | 11,023 | 0.12x | 0.016 | ahead | 10,454, 10,478 |
| `time.sleep_late.1ms` | 6,192 | 88,402 | 0.07x | 24,312 | 0.25x | 0.016 | ahead | 11,191, 11,652 |
| `time.sleep_late.1us` | 1,400 | 61,020 | 0.02x | 13,497 | 0.10x | 0.016 | ahead | 13,497, 12,615 |
| `time.timer_1ms.late` | 49,110 | 74,260 | 0.66x | 24,250 | 2.03x | 0.016 | behind | 11,126, 11,601 |
| `wake.periodic.idle` | 20,426 | 73,658 | 0.28x | 20,134 | 1.01x | 1.000 | at parity | 10,810, 11,027 |
| `wake.periodic.loaded` | 4,403 | 59,212 | 0.07x | 9,156 | 0.48x | 0.413 | unresolved | 4,339, 4,414 |

Against Linux's default slack every row counts ahead on both benches.
Against the slack-free Linux, four are behind, each with every boot later
than every Linux run:

- T14 `time.sleep_late.100us`, 5.2 against 1.8 µs, 2.85x;
- T14 `wake.periodic.loaded`, 3.6 against 2.4 µs, 1.53x;
- T14 `wake.periodic.idle`, 20.8 against 17.7 µs, 1.18x;
- AMD `time.timer_1ms.late`, 49.1 against 24.3 µs, 2.03x, and 4.3x against
  the two TSC runs' 11.1 and 11.6 µs.

The three T14 rows read 4.4, 3.0 and 17.8 µs under ToyOS on 2026-10-28
(evidence branch `scoreboard-2026-10-28`), so each is later now than then;
nothing saw that, because the rows counted ahead. On the AMD bench
`wake.periodic.idle` reads 6.2 µs in two boots and 35 µs in two
(`issues/many-osbench-rows-on-the-amd-bench-are-bimodal-per-boot.md`) and
`wake.periodic.loaded` is level with the TSC runs. What a late wake spends
its time on in any of the four is unmeasured.

**Exit**: every timer row within Linux's `.slack1ns` variant on both
benches, with TSC-timed Linux runs on the AMD bench, in a labelled
current-state comparison, or each remaining difference measured to its parts
and justified.

**Owner**: the kernel's timers and the scheduler's wake path
(`kernel/src/sched/`).
