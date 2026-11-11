---
status: open
kind: finding
opened: 2026-11-11
---

# An AP takes IST vectors before its TSS

An AP's trampoline (`kernel/src/arch/x86_64/smp.rs`) loads the kernel's IDT
and enters Rust under the BSP's GDT. The AP loads its own GDT and TSS
(`ltr`) and its declared `CR4` only later, in `percpu::init_ap`. Until
`ltr`, TR is still what INIT left, so an NMI, `#DF` or `#MC` in that span
takes its IST stack pointer from no TSS of the kernel's. Until `CR4` is
applied, those three entries' `rdgsbase` and `wrgsbase` (`arch::entry`)
would also be `#UD`. Either way such an event ends the machine with no
report. Found by reading while making those entries independent of GS.

**Exit**: an AP holds its own GDT, TSS and declared `CR4` before it loads
the IDT.
