---
status: open
kind: tooling
opened: 2026-10-12
---

# The simulator rotates spawn placement from where the last spawn landed; the kernel from a counter

Both call `CpuHandles::place(start, now)`, the least-loaded scan, but they
choose `start` by different rules:

- the kernel, `kernel/src/sched/driver.rs`'s `placement`: a global counter,
  advanced by one on every spawn of any process, wherever the spawn landed;
- the simulator, `kernel/sim/src/vm.rs`'s `Vm::spawn`: one past the CPU the
  previous spawn landed on.

On a machine where the loads tie the two agree; wherever a spawn lands
somewhere other than its start, the next start differs, and so does every
placement after it. So which CPUs a spawn burst fills — the arrangement the
rest of a run is measured on — is a different sequence in the model than on
metal. PR #874's metal judgement turned on exactly that arrangement: its
scout's pair shared one CPU with a spinner of its own process on both
benches, which a scratch simulator scenario of the same phase (8 CPUs, 8
spinners, 6 seeds per tree) did not reach once.

**Exit**: the simulator chooses `start` by the kernel's rule, from one
definition both call.
