---
status: open
kind: tooling
opened: 2026-08-14
---

# Every timestamp in the kernel assumes an AP's TSC does not trail the BSP's, and nothing measures it

`clock::nanos_since_boot` subtracts one global `TSC_BOOT` — stored by the BSP
during `clock::init` — from whatever `rdtsc` this CPU answers. A CPU whose TSC
reads below that value has no honest answer to give.

The assumption has been stated out loud, and `kernel/src/clock.rs` carries it:

> **Cross-CPU ordering rests on the TSC being invariant and firmware-synchronised.**
> ToyOS targets 2020+ x86-64, where it is. If it is not, two records from two
> CPUs may be merged out of order by the skew; nothing breaks and the reader
> cannot tell.

That is an accurate description of a *small* skew. It is not a description of a
CPU whose TSC starts behind the BSP's by more than the boot took, which is the
case the subtraction cannot represent at all.

## What changed, and what did not

Until 2026-08-14 the subtraction was `rdtsc() - TSC_BOOT`, and with
overflow-checks on — which every guest build has — a trailing TSC **panicked the
kernel**. `log::emit` then began reading the clock inside its publication
bracket, where such a panic would fire with `IF` clear and a reservation taken
and uncommitted, and the panic handler's own `log!` would reenter the same
shard. The subtraction is `saturating_sub` now, so the failure is a timestamp of
zero instead of a fault.

**That makes it survivable, not correct.** A CPU stuck behind stamps every
record it writes with zero for as long as it trails, and those records read as
the oldest thing the machine has: `log/read.rs`'s merge sorts them last, and its
per-shard descent — which relies on `at_ns` descending with the sequence number
— stops at the first of them. A bracketed report (Ctrl+Alt+D) would then carry
nothing from that CPU. Every other consumer of `nanos_since_boot` on that CPU is
wrong by the same amount: deadlines, the scheduler's accounting, `at_ns` in
`/log`.

## What is not known

Nothing in this tree has ever measured TSC agreement across CPUs. Both halves
are open:

- **Does it happen on the T14?** `smp` brings APs up and no code compares their
  TSC to the BSP's, before or after. One `rdtsc` per CPU logged at the end of
  `init_ap` would answer it, and the metal session is where that has to run —
  QEMU's TSCs are synthesised from one host clock and will agree whatever the
  firmware does.
- **What should happen if it does?** The choices are a per-CPU `TSC_BOOT`
  captured at AP bring-up (which fixes the arithmetic and leaves cross-CPU
  comparison meaningless), `IA32_TSC_ADJUST` written to align them (which is
  what firmware is supposed to have done), or accepting the skew and saying so
  where records are merged. Not this branch's call.

**2026-08-25, promoted to `defect`.** "Nothing would tell us" is the defect:
`kernel/src/clock.rs` cites this file for the assumption its whole arithmetic
rests on, and the instrument that would confirm or refute it does not exist —
one `rdtsc` per CPU logged at the end of `init_ap` is the entire measurement.
QEMU synthesises every guest TSC from one host clock and cannot answer, so this
belongs on the metal session's table beside the control-register delta
(`issues/hardware/a-metal-session-runs-a-pre-flash-gate-first.md`). Owed by
whoever prepares that session: build the log line first, then read it.
