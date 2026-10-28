---
status: assigned
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

**Exit**: on the T14, since a KVM guest's idle is its host's: the idle
minute's package energy, timer lateness no worse than `hlt`'s by more than
the chosen state's exit latency, and osbench's `cpu.clock` maximum at ratio
42 (4185 to 4190 MHz, as Linux's reads it on the same T14) in every group
where Linux's reaches 42.
**Mutation**: always the deepest state. **Oracle**: Linux's figures.
