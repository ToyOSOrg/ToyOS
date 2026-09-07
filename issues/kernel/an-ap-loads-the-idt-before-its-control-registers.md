---
status: open
kind: defect
opened: 2026-09-07
---

# An AP loads the IDT before its control registers, so a fault there triple-faults

`percpu::init_bsp` applies the control registers before it loads the IDT: every
entry stub in `kernel/src/arch/idt` saves SSE state, and `fxsave` without
`CR4.OSFXSR` is `#UD`, so a fault taken between the two would raise `#UD` inside
its own handler, double-fault into the same stub, and reset the machine. That
ordering is stated at the site.

**The AP path is the other way round and cannot be reordered the same way.** The
trampoline's long-mode half sets `CR4.PAE` and nothing else, then sets the GS
base, then `lidt`s the kernel's table — and `CR4.OSFXSR` is not set until
`percpu::init_ap` reaches `control_regs::init`, several Rust statements later.
Between them an AP runs `control_regs::init_cr0`, `pat::init` (MSR writes and a
CR0 round trip) and `mm::paging::load_kernel_flush` (a CR3 reload) with a table
loaded whose every stub is `#UD` on entry. Any exception in that window is a
triple fault with nothing on any channel.

Latent rather than active: the span is straight-line code over memory that is
already mapped, and the T14's own stop is on the BSP, before `boot_aps` runs at
all. Nothing has been observed to fault there.

Neither obvious fix is available without a ruling:

- **Set `CR4.OSFXSR` in the trampoline before the `lidt`.** This is the direct
  fix and it puts a second decider on a control register, which `CLAUDE.md`
  forbids by name — "a CPU's control registers come from one declaration".
- **Move the `lidt` out of the trampoline into `init_ap`, after
  `control_regs::init`.** This matches the BSP exactly, but it needs an
  `idt::load` separate from `idt::init` (which fills the table and is BSP-only),
  and it leaves the same span with *no* table rather than an unusable one — the
  same fatal outcome, honestly spelled, but not an improvement by itself.

**Exit condition**: one of the two above, chosen by whoever owns
`arch::control_regs`' one-declaration rule, with `smp_bringup` and the SMP
suite as the judge. A guest cannot currently produce the fault — an actuator
that raises `#UD` between the trampoline's `lidt` and `init_ap` would be the
negative control, and does not exist.
