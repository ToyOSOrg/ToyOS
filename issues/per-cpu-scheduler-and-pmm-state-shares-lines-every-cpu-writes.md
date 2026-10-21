---
status: open
kind: defect
opened: 2026-10-12
---

# Per-CPU scheduler and PMM state shares lines every CPU writes

Three structures put what each CPU writes, on its hot path, on a line or a lock
every other CPU writes too:

- **The PMM lock, per user copy.** Every user-copy window pins the frames it
  covers and unpins them after (`kernel/src/user_ptr.rs:301-309`), and each
  is a pass over the one global `BITMAP` lock
  (`kernel/src/mm/pmm.rs:98`, `pin_range` and `unpin_range` at `:249-279`):
  two acquisitions of the machine's one frame-allocator lock per copied run,
  on every syscall that copies.
- **`CpuHandle`.** Unpadded (`kernel/pure/sched/cpu.rs:2136-2175`) in one
  boxed slice (`cpu.rs:2290`), 32 bytes by its fields without `sched-check`,
  so two CPUs' handles share a line. Each pass stores its own `load`, `surplus` and `last_pass`
  (`cpu.rs:1669-1672`), and every wake does a read-modify-write on its
  target's doorbell (`kernel/pure/sched/mailbox.rs:475`).
- **`Frontier`.** One global `AtomicU64`, `fetch_max`ed on every pick
  (`kernel/pure/sched/fair.rs:44-46`, called at `cpu.rs:1827`).

**Measured on the T14** (`wt/toyos-scout-clock` at `fa6a2a829`, the
`internet_download` row's twelve download windows over two boots;
https://github.com/ToyOSOrg/ToyOS/pull/866#issuecomment-6119513916, logs in
the parts it links):

- The PMM lock's own spin is 0.33 to 0.39 % of C0 time.
- HITM traffic is bounded at 2.12 to 2.99 % of C0 time: forwarded loads and
  RFOs, times the 100–101 ns a control's line hop costs. The bound is loose
  three ways: a control hop is a load plus its loop, so it overstates one
  HITM; the counters cover every CPU, both rings and every shared line, not
  only these four; and the load event (D2H/04H) on this CPU also counts clean
  forwards.

Intel's optimization manual has critical data and locks allocated apart,
aligned to no less than the false-sharing threshold, the CLFLUSH line size of
CPUID.01H:EBX[15:8] (248966-051, Vol. 1 §11.4.5, User/Source Coding Rule
17), which backs the stage on the T14. On the AMD bench the stage stands on
ToyOS's rule that per-CPU data goes on lines of its own, resting on the line
AMD documents: 8 quadwords by CPUID.01H:EBX[15:8] (PPR 55922 rev. 3.06,
`CPUID_Fn00000001_EBX[15:8]`), and 64 bytes (Software Optimization Guide for
AMD Family 17h Processors, 55723 rev. 3.00, §2.6.1).

Nothing in the run attributes a miss to the `BITMAP` lock, `IN_PASS`,
`CpuHandle` or `Frontier`, and the AMD bench was not read. The next
measurement attributes the HITM count to the lines it lands on, on both
benches.

A fourth, `IN_PASS`, was measured with them and now sits in its CPU's own
record (`kernel/src/cpu_local.rs`), a line no other CPU writes.

**Exit**: that attribution, posted, and read again after the change; and
each of the three either written per CPU on a line no other CPU writes, or
kept with its refusal reason at its site.
