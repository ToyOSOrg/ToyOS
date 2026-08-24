---
status: open
kind: defect
opened: 2026-08-08
---

# 18 `gs:[N]` literals in five entry stubs are still bound to nothing

The owner's own review note, and the one finding in it that names a hazard
rather than a shape: *"this verifies against constants but doesnt gaurantee
that the constants are the same used in for example preempt.rs."* He is right,
and he is right about less of the tree than he was.

**The fix shape has landed for everything written in Rust.**
`arch/percpu.rs:234-264` declares 18 `OFF_*` constants, each
`offset_of!(PerCpu, f)`, and every GS access this kernel writes in Rust now
feeds one into the assembly as a `const` operand — the `const`-generic
primitives at `:323-422`, `reserve_log_slot`'s four-word read at `:671-674`,
`preempt`'s six accessors, `irq_census::irq_took!` deriving both of its
displacements from `OFF_IRQ_COUNTS`, and `arch/idt/nmi.rs`'s **naked** entry
reaching `nmi_active` as `gs:[{active}]` with `active = const OFF_NMI_ACTIVE`.
That last one is the proof the rest is reachable: a naked stub takes `const`
operands, and `ring3_naked_asm!` is `($($body:tt)*)` — it passes its body
through and appends two of its own, so a caller may write one at the site with
no macro plumbing at all.

**What is left is the entry stubs, and they are the paths that punish it.**
Measured 2026-08-24 at `f62a6443`: 54 `gs:[` lines across ten files, of which
19 are `const` operands, 17 are prose, and **18 are hand-written literal
displacements across five files** —

| file | literals | offsets |
|---|---|---|
| `arch/syscall.rs:268-289` | 8 | 16, 24 (twice), 216, 224, 232, 240 (twice) |
| `arch/idt/timer.rs:40-80` | 4 | 244, 248, 260 (twice) |
| `arch/idt/device_irq.rs:39,45` | 2 | 240 |
| `arch/idt/mod.rs:413,417` | 2 | 240 |
| `arch/idt/tlb.rs:25,34` | 2 | 240 |

nine distinct offsets in all. These are the `SYSCALL` entry's stack switch and
its three diagnostic stores, the Ring 0 timer's re-arm and `need_resched`, and
every IRQ entry's preempt-count open and close.

`arch/percpu.rs:266-285` carries **20** `const _: () = assert!(...)` — the count
grew with the constants rather than shrinking. Eighteen assert an `OFF_*` against
a literal; two (`kernel_rsp == 16`, `tss == 32`) assert `offset_of!` directly,
because no constant is declared for a field no Rust GS access names. A third
copy of the same numbers still lives in the field comments (`cpu_id: u32, //
offset 8`): 27 `// offset` lines at `:79-147`.

So the hazard is unchanged in kind and smaller in extent. Reordering a field
trips the asserts; changing one asserted literal and its field together does
not, and the 18 remaining asm sites then read the wrong bytes with no
diagnostic at all. The fix is the same one, finished: feed `offset_of!` into
those five stubs as `const` operands, and the 20 asserts delete with the last
literal.

Two smaller PerCpu items ride this and **must not precede it**, because field
surgery before the unification is what the remaining copies punish. Both
re-verified 2026-08-24:

- `lapic_id` (`percpu.rs:81`) has zero readers. Written at `:585`; every other
  `lapic_id` in the kernel is the *parameter* of `alloc_percpu`, `init_bsp` or
  `alloc_ap`, or `:947` logging that parameter. Delete the field.
- `alloc_percpu` (`:567`) sets 8 of `PerCpu`'s 22 real fields — 26 declarations,
  four of them padding — and relies on `alloc_zeroed` for the other 14. One
  total `ptr::write(PerCpu { .. })` says what the struct is. The
  `current_tid`/`current_pid` `u32::MAX` sentinel stays — it is an asm wire
  format and is already `Option`-decoded at the boundary.
