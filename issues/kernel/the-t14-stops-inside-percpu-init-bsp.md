---
status: open
kind: defect
opened: 2026-09-06
---

# The T14's boot stops inside `percpu::init_bsp`, after `control_regs`

Run 4 (metal `e6494990`, parameters `watchdog` and `early-panel`) painted its
last record and never painted another:

```
[0.000 cpu0 boot] control_regs: cpu0 cr0=0x80010033 cr4=0x00320e68 efer=0x0d01 smep smap pcid umip nx
```

`loader.log` shows the handoff completed, so the kernel was entered and ran at
least to that line.

## Where, and why it is that narrow

The window is **not** `main.rs`'s phase list. `control_regs::init(0)` is called
from `percpu::init_bsp` itself (`arch/percpu.rs`), and the next record in
program order is that same function's own `percpu: BSP cpu_id=0 lapic_id=…` —
separated only by `fpu::init()`, the `wrmsr` that makes `gs:` valid, and the
`PERCPU_READY` store. That line is absent, so nothing after it ran, and every
later candidate the window seemed to hold — the I/O APIC, `sti`, `syscall::init`,
the HPET — is downstream of a stop that already happened.

Two things that would have widened the window are ruled out from the tree:

- **The panel can see the next record.** `alloc_log_shard` gives cpu0 the boot
  shard itself, so the record after `PERCPU_READY` goes to the shard the panel
  already renders; `log::emit` repaints after every record while `EARLY` holds.
  A boot that got further would have painted further.
- **It hung; it did not fault.** No IDT is loaded yet, so a fault here is a
  triple fault, and a triple fault resets the machine. The panel still held the
  same stale text 14+ minutes later and needed the power button, so the CPU
  never reset.

## What is left, and how the next run says which

`fpu::init()` is the first suspect and the only one that touches state QEMU
models differently from Tiger Lake: `load_initial` (`fninit`, `ldmxcsr`) then
`self_check`, whose `UserFpState::saved_from_cpu()` reads the CPU's
architectural default. Its assertion message calls `percpu::cpu_id()`, which
reads `gs:` — and `gs:` is not valid until the `wrmsr` three lines later, so on
a machine where that assertion *fails* the failure path itself faults before it
can report. The `wrmsr`, the store, and the first `emit` past `PERCPU_READY` are
the rest.

`fpu::log_state` now runs between `fpu::init` and the `wrmsr`, rather than after
this function's own line, on the pre-`PERCPU_READY` boot-shard path that
`control_regs` proved reaches the panel. It takes its CPU id as an argument
instead of reading `gs:`, which is what lets it run there at all — and which
removes the same latent hazard from the AP path. It prints this CPU's extended
state, so it is a machine fact rather than a breadcrumb, and the next photograph
reads:

- no `fpu:` line — the stop is inside `fpu::init`, whose `xcr0` was never reached
- an `fpu:` line, no `percpu: BSP` — the `wrmsr`/store/first-`emit` group
- neither, still ending at `control_regs` — `control_regs::init`'s own tail

No record was added before `fpu::init`: `control_regs::init` has no wait after
its own printed line, so a record there would discriminate nothing.

The steps that produced no record now print what they established: `idt::init`
the table it loaded and how many vectors carry only the unclaimed stub, the
`sti` its `RFLAGS` read back, `syscall::init` the three MSRs the CPU holds, and
`clock::init` the HPET's period and its counter's first reading before it waits
on that counter.

**Exit condition**: one metal run whose panel shows which of those records is
last. The T14 is the only judge — QEMU boots this path on every test.

Related: `issues/hardware/an-armed-tco-has-never-reset-the-t14.md` is why this
wedge needs a hand on the power button instead of resetting itself.
