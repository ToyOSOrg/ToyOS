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

**Exit**: each benchmark runs after a steady-state warm-up of the CPUs it
uses and prints the chunk time measured beside it, and a T14 run on each
system shows `cpu.chunk` agreeing with the all-CPU rate per CPU within the
run-to-run spread.
