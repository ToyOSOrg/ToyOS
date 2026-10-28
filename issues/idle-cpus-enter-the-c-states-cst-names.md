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
On the T14 that is `MWAIT 0x31` and `0x60` (253 and 1048 µs); on the AMD
bench port 0x414 (400 µs), its type-3 0x415 withheld
(`issues/the-amd-benchs-c3-is-withheld-for-want-of-a-coherence-statement.md`).
AArch64 keeps `wfi` (`issues/aarch64-cpus-idle-only-in-wfi.md`). What
remains is the exit below, read on metal.

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

**The exit cost of deep states, accepted** (orchestrator, 2026-10-28, under
the owner's direction of 2026-10-27), as #1073's first boots read it at
`59dcd1f03`, p50 against main's `ea6414e53` and against Linux 6.8 on the same
bench (Linux's 50 µs timer slack included):

| row | T14 main | T14 deep idle | T14 Linux | AMD main | AMD deep idle | AMD Linux |
|---|---|---|---|---|---|---|
| `time.sleep_late.1ms` | 17.7 µs | 22.9 µs | 71.5-71.8 µs | 32.5 µs | 5.4 µs | 69.7-70.8 µs |
| `time.sleep_late.10ms` | 17.8 µs | 29.5 µs | 213 µs | 32.6 µs | 32.3 µs | 342-344 µs |
| `wake.storm.drain.w4` | 37 µs | 127 µs | 121-123 µs | 105 µs | 307 µs | 77-78 µs |
| `wake.storm.drain.w16` | 77 µs | 144 µs | 370-1041 µs | 333 µs | 329 µs | 113-200 µs |

A sleep long enough for a deep state is woken early by its exit and still
lands later the deeper the state was (none at 100 µs, C2 at 1 ms, C10 at
10 ms), which an early wake that fell short would make hundreds of µs:
consistent with the cost of the caches a deep state powers off, not measured
as that. A burst of wakes to CPUs in deep states pays their exits.

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
  nothing beside osbench for more than 0.9 ms of the 5.5 ms window. Mem
  stays unexplained. Net, procs, tail and nvme reached 4189 under the
  always-deepest control, so the history holding idle CPUs shallow after
  the group before is the open hypothesis there; a per-CPU state read
  inside the window would test it.
- Group starts at 4189 across the eight T14 boots with the deep states:
  8 and 8 (r2), 8 and 10 (r3), 10 (B4), 5 (B5), 10 (B6) and 10 (B7), counted
  as a maximum above 4100 MHz. Only sched, files, ipc and work reached it
  in all eight; B5 and B6 lost time, which the others had. It is a spread
  from boot to boot, not a fixed set of groups.
- Soundserver's worst IRQ lateness, over every window line of the
  testcases boots: with every idle CPU in C1 (one boot a bench), T14 181 to
  572 µs and AMD 0 to 2661; with the deep states (two boots a bench), T14 0
  to 564 µs and AMD 0 to 2661. The deep states move neither, and no window
  underran.

**#1066's exit is unmet** where this track stands: the T14's `cpu.clock` reaches
ratio 42 at 5 to 10 of 17 group starts against Linux's 13. Of the groups
below it, boot, identity, energy and system are held by acpiserver's
measured start-up work; the rest are the open question above. Three
diagnostic boots (#1073's `r6/`) read the counters around each group's
clock, the same with the history switched off, and the scheduler's diary
across it. The counters read came back as one reading twice in 13 of 17
windows (`issues/a-counters-read-within-10-ms-of-another-returns-the-older-reading-as-new.md`),
and switching the history off left the count inside the spread. The
finding goes into this record with the track's next change.

**Exit**: on the T14, since a KVM guest's idle is its host's: the idle
minute's package energy, timer lateness no worse than `hlt`'s by more than
the chosen state's exit latency, and osbench's `cpu.clock` maximum at ratio
42 (4185 to 4190 MHz, as Linux's reads it on the same T14) in every group
where Linux's reaches 42.
**Mutation**: always the deepest state. **Oracle**: Linux's figures.
