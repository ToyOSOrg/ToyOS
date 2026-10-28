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
measuring CPU until every reading of a dependent `add` chain for 50 ms
agrees within 3% (`osbench_rules::settle`), or for 0.5 s, and every result
line prints that clock as `mhz_settled`, the clock a second settle reaches
after the row as `mhz_after`, and the clock just after as `mhz`; each group
also prints `cpu.clock`, 200 probes from idle. A reading is the difference
of a short add chain's time and a long one's, so the cost of reading the
time is no part of it. Three probes in a row, the rule until 2026-10-27,
took the AMD bench's `schedutil` step for the settled clock: its Linux runs
of 2026-10-24 settled at 1,357 MHz. On 2026-10-28 every settle of the AMD's
Linux ran out its time under a 1% rule (84 of 84) where ToyOS's on the same
CPU never did (0 of 92). That Linux timed through the HPET, whose 1.2 µs a
read sat inside a 25 µs probe, reading the clock some 5% low and moving it
by the read's jitter.

**Exit**: a T14 run on each system shows `mhz_settled` near the all-CPU rate
per CPU within the run-to-run spread, or names the benchmark whose CPU did
not settle.
