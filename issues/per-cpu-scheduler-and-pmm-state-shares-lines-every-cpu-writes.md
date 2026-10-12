---
status: assigned
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

None has been measured. `wt/toyos-scout-clock` holds the measurement: the
contention each costs on the T14 and the AMD bench.

**Exit**: that measurement, posted; and each of the four either written per
CPU on a line no other CPU writes, or kept with its refusal reason at its
site and its measured cost on both benches in the closing commit.
