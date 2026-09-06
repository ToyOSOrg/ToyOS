---
status: open
kind: defect
opened: 2026-09-06
---

# `short_sleep_livelock` stalls on CI with one sleeper never returning, and is green alone

Merge-queue `ci` run 33999755256 (the queue's composition of #424 on main
e9731380), job 101396692597 `guest (3)`, KVM, one guest per machine:

```
FAIL short_sleep_livelock: a sleep shorter than one LAPIC tick took the CPU with it: STALLED: 63s of guard expired, and the guest had said nothing for the last 63s of it
...
[kernel ... cpu1 tid=1] exit: test_rs_abuse_short_sleep tid=1 code=0 cpu=7ms
sleeps of 100000 ns returned
[kernel ... cpu1 tid=3] exit: test_rs_abuse_short_sleep tid=3 code=0 cpu=7ms
sleeps of 100000 ns returned
[kernel ... cpu1 tid=5] exit: test_rs_abuse_short_sleep tid=5 code=0 cpu=6ms
sleeps of 100000 ns returned
[kernel ... cpu1 tid=7] exit: test_rs_abuse_short_sleep tid=7 code=0 cpu=6ms
  STALL short_sleep_livelock  (69s)  — the guard expired, so this says nothing about the tree
  ALONE short_sleep_livelock: GREEN, and it was alone both times
```

Four of the program's sleepers finished their 100000 ns round and exited on
cpu1; the fifth never printed its return and the guest said nothing more for
63 s. That is the shape the test exists to catch, seen once on the CI
instrument; the redlist row (`src/redlist.rs`, `short_sleep_livelock`,
`Instrument::Ci`) records it, and the owner is the sleep path in
`kernel/src/sched` that the test's write-up (`tests/toyos.rs`,
`short_sleep_livelock`) names.
