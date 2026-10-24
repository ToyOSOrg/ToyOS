---
status: open
kind: tooling
opened: 2026-10-21
---

# osbench times CPUs leaving idle, not their settled clock

`tests/guest/osbench` runs each benchmark straight after the last one, and
its clock gauge, `cpu.chunk`, is a 2 ms single-threaded burst at the start
of each group. So both read a CPU on its way out of idle, and how fast it
gets there is each system's idle and frequency policy rather than the
workload.

Measured on the T14 (osbench at 951c7d1, Linux with intel_pstate powersave
and EPP balance_performance, ToyOS with HWP at EPP 0x80, idling in `hlt`): the
same chunk took 10.5 µs on Linux in every group, and 7.9 µs, then 3.3 µs,
across ToyOS's groups. Under all-CPU load the two run one clock: 2.389 M and
2.371 M chunks/s machine-wide. Dividing a result by its group's `cpu.chunk`
is therefore no normalisation: the memory bandwidths, equal raw, come out a
quarter apart.

`cpu.chunk`'s loop compiles to the same 3,125 dependent multiply-adds on
both builds (`llvm-objdump` of `chunk_rate` in each), so its ratio is a
clock ratio: at the four cycles a step takes, 10.5 µs is about 1.2 GHz and
3.3 µs about 3.8 GHz, an inference from the instruction latencies and not a
counter reading.

Since round 2 every benchmark but the wake and fairness groups' (whose CPUs
idle or are all busy by design) runs after `clock::settle`, which spins the
measuring CPU until three probes of a dependent `add` chain agree within 1%,
and every result line prints that clock as `mhz_settled` and the clock just
after as `mhz`; each group also prints `cpu.clock`, 200 probes from idle.

The network runs show it again. On the T14 at `d99ef5a35`, each group's
`cpu.clock` p50 in MHz was:

| | ToyOS (3 runs) | Linux (4 runs) |
|---|---|---|
| before `lan` | 2791, 2791, 1495 | 4186, 4184, 4185, 4186 |
| before `internet` | 3189, 1595, 2990 | 3790, 4186, 3788, 4186 |

Both groups wait mostly on the network. In those runs ToyOS's settled clock
also ranged from 1.6 to 3.8 GHz across rows
(`ToyOSOrg/evidence` branch `pr-1065`, `r3/`).

**Exit**: a T14 run on each system shows `mhz_settled` near the all-CPU rate
per CPU within the run-to-run spread, or names the benchmark whose CPU did
not settle.
