---
status: open
kind: finding
opened: 2026-11-05
---

# Sixty-four thread spawns in a row landed on the spawner's own CPU, where placement prefers the other

`acpi_mediated`, before #1187 held its asker to the last CPU, spawned a
thread up to 64 times from its main thread on cpu0 of a two-CPU q35 guest,
joining each, until one read itself off cpu0. In four runs all 64 ran on
cpu0 (`acpi: 64 threads in a row started on the boot processor`): nightly
39566825102 (tcg), 40847028162 and 41618038084 (kvm), and guest-rows
42008942191 on main at 15c26bfbc (kvm), where the 64 took about 15 ms.

**The rule it contradicts.** A spawn is placed by `CpuHandles::place`
(`kernel/pure/sched/cpu.rs`) from a start that rotates per spawn
(`placement`, `kernel/src/sched/driver.rs`), by the least of `(load,
!sleeping_unrung, busy_siblings, homed, away, idle_exit)` among answering
CPUs. With equal published load an idle cpu1 wins on `sleeping_unrung`, and
otherwise on `homed`, since the spawner's CPU is home to the process's main
thread; a remaining tie alternates with the start. Sixty-four landings on
cpu0 in a row are systematic.

**Candidates, none established:**
- cpu1's published load stayed above cpu0's for the whole burst;
- cpu1 read as not `answering`: a doorbell edge standing past
  `STALE_PASS_NS`, so the scan left it out. A wake signal left unserviced
  that long is a kernel defect, and this record becomes a `defect` with
  severity `hang`;
- each thread went to cpu1 and was handed to cpu0 before its first
  instruction: the spawner blocks in `join` at once, so cpu0 turns idle and
  asks to steal while cpu1 is still waking from its halt
  (`answer_steal_requests`).

No row arranges this any more: the probes pin. The cheapest reproduction is a
guest binary that spawns and joins in a loop from cpu0 and counts where each
thread's first `current_cpu()` lands, with the scheduler's trace diary read
after it.

Owner: `kernel::sched`'s placement, held by the orchestrator.

**Exit**: the cause named from a reproduction, and fixed if it is a defect;
or the rule shown to put these spawns on cpu0, and its documentation made to
say so.
