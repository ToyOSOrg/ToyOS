---
status: open
kind: tooling
opened: 2026-10-01
---

# The control registers verdict's doc comment sits on the census verdict

In `tests/toyos.rs`, the doc block that starts "Every CPU's `CR0` and `CR4`,
against what a CPU running this kernel must hold" runs straight into
"The interrupt census adds up, …". Both paragraphs render as the doc of
`irq_census`. `control_regs`, which the first block describes, has no doc
comment. Nothing checks that a doc comment names the item it is attached to,
so rustdoc and a reader both credit the census verdict with the
control-register rules.

**Exit:** the block sits on `control_regs`, and `irq_census` carries only its
own doc.
