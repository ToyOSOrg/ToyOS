---
status: open
kind: track
opened: 2026-09-29
---

# Idle CPUs enter the C-states `_CST` names

Built (#1073): acpiserver hands the kernel what each
processor's `_CST` names that it may enter as it is
(`system/acpiserver/src/cst.rs`), the kernel checks it against the CPU
(`lib/userbound/src/idle.rs`) and chooses a state each idle period
(`kernel/pure/sched/idle.rs`, `kernel/src/idle.rs`), and a `WakeLatency`
hold bounds the exit (`soundserver` holds half a period while it plays).
What is handed over is a rule per vendor and family: Intel's FFH `MWAIT`
states, and trapped port reads off Intel, of type 3 only on AMD from Family
17h and on Hygon. On the T14 that is `MWAIT 0x31` and `0x60` (253 and
1048 µs); on the AMD bench, Family 17h, ports 0x414 and 0x415 (400 and
350 µs). AArch64 keeps `wfi` (`issues/aarch64-cpus-idle-only-in-wfi.md`).
What remains is the exit below, read on metal.

Not established for the AMD rule: whether any Zen part needs BM_STS read
before a type-3 entry, which ToyOS does not do, or BM_RLD set, which ToyOS
leaves as firmware set it; only PPR 55901 Vol. 5 speaks to them, for Family
19h Model 11h (BM_STS "no longer used except for server CPU", BM_RLD "no
longer needed").

Before it, ToyOS's cores never went below C1/C1E and its package never left
PC0, and its idle package drew 3.16 W against Linux's 1.18 to 1.57 W (#866,
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119513916).
What `hlt` alone costs a wake, firmware's C1E promotion included, is
`issues/a-cpu-leaving-hlt-runs-its-first-microseconds-slow-under-c1e.md`.

On the T14 `hlt` also holds a one-thread load to the turbo ratio for three or
more active cores, 38 against 42
(`issues/toyos-uses-what-modern-hardware-offers-for-speed.md`). PR #1066's
scout, every idle wait `mwait 0x31` (the T14's `_CST` C2), read `cpu.clock`'s
maximum at 4189 MHz in 13 of 17 osbench groups and at 3789 in fair, net,
procs and mem, unexplained, and ran `crunch.single` 1.10 to 1.11 times
faster than a `hlt` control. Its `work.build_shaped.j1` p50 read 1176 ms
against the control's 967, with its own 5 runs spread from 817 to 1594 ms,
the control on a different `main`: unresolved.

**The exit cost of deep states is a tradeoff to bring to the architect, not
accepted.** It was recorded as accepted on ratios alone (1.5 to 3.4 times),
which accepts no material compromise. In absolute terms, p50 in µs with every
idle CPU in C1 (two A\* boots) against the deep states (two B3 boots), one
build (`f6510defd`) and one session a bench, B7 at `18fe4e150` beside them,
and Linux 6.8 on the same bench (its 50 µs timer slack included; #1073's
first boots):

| row | T14 C1 | T14 deep | T14 delta | T14 Linux | AMD C1 | AMD 0x414 | AMD 0x415 | AMD Linux |
|---|---|---|---|---|---|---|---|---|
| `time.sleep_late.1ms` | 17.7, 17.8 | 22.1, 14.6 (B7 17.2) | −3 to +4 | 71.5-71.8 | 32.2, 32.5 | 5.6, 5.6 (B7 5.4) | 5.5 | 69.7-70.8 |
| `time.sleep_late.10ms` | 17.8, 17.8 | 28.3, 28.3 (B7 27.4) | +10.5 to +10.6 | 213 | 32.3, 32.6 | 32.6, 33.3 (B7 32.8) | 52.4 | 342-344 |
| `wake.storm.drain.w4` | 38.5, 37.6 | 126.1, 128.6 (B7 127.9) | +88 to +91 | 121-123 | 306, 107 | 112, 306 (B7 107) | 64.0 | 77-78 |
| `wake.storm.drain.w16` | 74.1, 77.6 | 161.1, 145.5 (B7 130.1) | +68 to +87 (B7 +52) | 370-1041 | 304, 328 | 321, 331 (B7 350) | 80.3 | 113-200 |

The AMD's 0x415 column is one benchcase boot of #1089 at `31db18c19`, where
every deep period requests 0x415 (its exit and residency are both below
0x414's). It is on a later `main` than the columns beside it, with main's
scheduler placement change (`05da59f65`) between them. Against 0x414, a
1 ms sleep lands as it did, and a 10 ms sleep lands 20 µs later (52.4
against 32.6 to 33.3), consistent with a deeper state's exit and not measured as that. The storms drain
faster than in either earlier column, but no control on the same base was
read, so that is not attributed to 0x415.

A sleep long enough for a deep state is woken early by its exit and still
lands later the deeper the state was (none at 100 µs, C2 at 1 ms, C10 at
10 ms), which an early wake that fell short would make hundreds of µs:
consistent with the cost of the caches a deep state powers off, not measured
as that. A burst of wakes to CPUs in deep states pays their exits. The cost
is the T14's. On the AMD, 0x415 lands a 1 ms sleep 27 µs sooner than C1 and
a 10 ms sleep 20 µs later, and its storms are not attributed (above).

What it costs, as inference, not measured per workload:

- **Interactive**: an input event that wakes an idle CPU on the T14 lands up
  to about 10 µs later, and one that fans out to 4 to 16 sleeping threads
  about 70 to 90 µs later, against a 60 Hz frame's 16.7 ms.
- **Audio**: while a stream plays, soundserver holds the exit to half its
  2.9 ms period, about 1.45 ms, which both T14 states (253 and 1048 µs) and
  the AMD's (400 and 350 µs) are inside: so a period's wake may still pay up to
  1048 µs of exit on the T14, a third of the period. Measured, its worst IRQ
  lateness is not moved and no window underran (below).
- **Normal workloads**: a server or build that wakes many sleeping threads
  in bursts pays the storm's 70 to 90 µs per burst on the T14, latency
  rather than CPU time; throughput rows did not lose (below).

**Where the exit stands** (#1073's second boots at `f8749dc69`, two boots
each against its control with the deep states off, all else equal):

- T14 `crunch.single` 1.10 to 1.11 times faster (deflate 60.3 to 54.5 ms,
  Linux's 54.6 to 54.7). The clock beside both deflate and
  `malloc_churn.1048576.t1` read 4187 to 4189 MHz in all eight boots with
  the deep states, against 3788 to 3789 in the four A\* boots (each row's
  `mhz`, `cpu.clock`'s reading beside it). Across deflate, every core but
  the thread's was 99.5 to 99.9 % of the row in core C7, which counts at the
  TSC's rate.
- T14 `cpu.clock` at 4189 at 8 and 8 group starts of 17, the control at
  none; Linux reaches 4190 at 13. Boot, identity, energy, system and mem
  stay at 3789 even when every idle CPU waits deepest (#1073's first
  always-deepest control), so idle depth does not hold them there;
  osbench now names what ran beside each group's clock
  (`<group>.cpu.clock.ran`).
- AMD: no clock gap and no throughput change; the IPC cost of the first
  boots is gone with the idle choice folded into the pass's timer. The slow
  deflate is not the idle states: it came back with every idle CPU in C1,
  at the same clock
  (`issues/the-amd-benchs-deflate-runs-6-percent-slow-in-some-boots-at-the-same-clock.md`).
- Third boots at `f6510defd`: the T14 at 4189 at 8 and 10 group starts.
  Boot, identity, energy and system run while acpiserver is still loading
  its tables (12.2 s by the kernel log's clock in the first B3 boot,
  `r4/` in the evidence), and acpiserver ran for the whole of each of those
  clock windows: a measured busy core beside the clock's thread. At the
  starts of net, procs, mem, tail and nvme still at 3789, the roster names
  nothing beside osbench for more than 0.9 ms of the 5.5 ms window.
- Group starts at 4189 across the eight T14 boots with the deep states:
  8 and 8 (r2), 8 and 10 (r3), 10 (B4), 5 (B5), 10 (B6) and 10 (B7), counted
  as a maximum above 4100 MHz. Only sched, files, ipc and work reached it
  in all eight; B5 and B6 lost time, which the others had. It is a spread
  from boot to boot, not a fixed set of groups.
- Soundserver's worst IRQ lateness, over every window line of the
  testcases boots: with every idle CPU in C1 (one boot a bench), T14 181 to
  572 µs and AMD 0 to 2661; with the deep states (two boots a bench), T14 0
  to 564 µs and AMD 0 to 2661. The deep states move neither, and no window
  underran. With 0x415 (#1089's first testcases boot, 6 window lines), AMD 0
  to 228 µs, and no underrun.

**`cpu.clock` at group starts** is a diagnostic of the turbo grant, not a
product target: the T14's reaches ratio 42 at 5 to 10 of 17 group starts
against Linux's 13, and #1066 set Linux's count as its exit. Three diagnostic
T14 boots at #1073's head read each group's clock window (`r6/` in the
evidence):

- With the counters read around each window (D1), 8 group starts reached
  4189; with the history also admitting every state (D2), 10. Both are
  inside the spread: the history does not hold the count down, and no
  idle-policy change is earned.
- D1's counters showed nothing. In 13 of 17 windows the two reads, 5 ms
  apart, were served one round, and the windows that did differ were read
  before osbench took a window from the readings' own stamps.
- The scheduler's diary across each window (D3), a boot in which boot,
  identity, energy, system and mem were at 3789: in the first four
  acpiserver ran beside osbench, 5.5 ms of the window by the roster. In mem
  the diary and the roster agree that no thread but osbench's ran on any
  CPU, and mem stays at 3789 under always-deepest too, so neither thread
  work nor the idle choice holds it. The diary counts a thread running from
  before the window only from its first event inside it. Net, procs, tail
  and nvme reached 4189 in this boot.

So boot, identity, energy and system are held by acpiserver's measured
start-up work. Mem's 3789 is open: interrupts, which no thread view sees,
or the package's power or thermal state after procs. The counters read
around mem's clock window is the instrument to read it with next, and no
idle predictor is tuned before it has been read.

**Exit**: on the T14, since a KVM guest's idle is its host's: the idle
minute's package energy, timer lateness no worse than `hlt`'s by more than
the chosen state's exit latency, and the exit costs above decided by the
architect. `cpu.clock` at each group start is read beside them as a
diagnostic, not held to ratio 42 for its own sake.
**Mutation**: always the deepest state. **Oracle**: Linux's figures.
