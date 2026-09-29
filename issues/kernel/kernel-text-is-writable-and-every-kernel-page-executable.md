---
status: open
kind: defect
opened: 2026-09-29
---

# Kernel text is writable, and every kernel page is executable

The kernel maps the whole direct map `PRESENT | WRITE` with no `NX`
(`kernel/src/arch/x86_64/paging.rs:946-949`), and the kernel image runs from
it, at `PHYS_OFFSET` plus its physical address. Its text can be overwritten,
and its heap, its stacks and every user page's direct-map alias can be
executed at CPL 0. Linux at `Ubuntu-6.8.0-142.142`, under the T14's
`CONFIG_STRICT_KERNEL_RWX=y`, makes text and rodata read-only and everything
else non-executable (`arch/x86/mm/init_64.c:1402-1447`), and under
`CONFIG_DEBUG_WX=y` walks the tables for a writable and executable mapping at
boot (`arch/x86/mm/dump_pagetables.c:434`). Two rows of the hardening table in
`issues/kernel/the-kernel-mitigates-what-linux-mitigates-on-the-t14.md`.

**Exit**: kernel text is mapped read-only and executable, rodata read-only and
`NX`, and every other kernel mapping `NX`; each boot walks the kernel tables
and panics on a writable executable leaf, and mapping the direct map as it is
today reds that walk.
