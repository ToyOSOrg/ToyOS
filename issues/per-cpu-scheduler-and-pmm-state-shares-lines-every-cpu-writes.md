---
status: open
kind: defect
opened: 2026-10-12
---

# Per-CPU scheduler and PMM state shares lines every CPU writes

Four structures put what each CPU writes, on its hot path, on a line or a lock
every other CPU writes too:

- **The PMM lock, per user copy.** Every user-copy window pins the frames it
  covers and unpins them after (`kernel/src/user_ptr.rs:301-309`), and each
  is a pass over the one global `BITMAP` lock
  (`kernel/src/mm/pmm.rs:98`, `pin_range` and `unpin_range` at `:249-279`):
  two acquisitions of the machine's one frame-allocator lock per copied run,
  on every syscall that copies.
- **`IN_PASS`.** One `AtomicBool` per CPU in one array
  (`kernel/src/sched/driver.rs:181`), so every CPU's flag is on one line, and
  each pass writes its own twice, a `swap` on entry and a `store` on exit
  (`driver.rs:194`, `:206`).
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
17). Nothing in the run attributes a miss to the `BITMAP` lock, `IN_PASS`,
`CpuHandle` or `Frontier`, and the AMD bench was not read. The next
measurement attributes the HITM count to the lines it lands on, on both
benches.

**Exit**: that attribution, posted, and read again after the change; and
each of the four either written per CPU on a line no other CPU writes, or
kept with its refusal reason at its site.
