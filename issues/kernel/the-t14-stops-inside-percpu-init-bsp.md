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

Two records now bracket those steps, both on the pre-`PERCPU_READY` boot-shard
path that `control_regs` proved reaches the panel:

- `percpu: cpu0 gdt loaded and control registers applied; the FPU's initial state is next`
- `percpu: cpu0 FPU initial state accepted; gs base and the per-CPU log path are next`

The next photograph names the step: stopping at the first means `fpu::init`,
stopping at the second means the `wrmsr`/store/first-`emit` group, and stopping
at `control_regs` still would mean `control_regs::init`'s own tail.

Three more cover the steps in `main.rs` that produce no record of their own
(`idt::init`, the `sti`, `syscall::init`), and one covers the HPET calibration.

**Exit condition**: one metal run whose panel shows which of the bracketed steps
is last. The T14 is the only judge — QEMU boots this path on every test.

Related: `issues/hardware/an-armed-tco-has-never-reset-the-t14.md` is why this
wedge needs a hand on the power button instead of resetting itself.
