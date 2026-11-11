---
status: open
kind: defect
opened: 2026-09-25
---

# A fault before the kernel's IDT resets the machine, and says nothing

On x86-64, from `_start` until `percpu::init_bsp` loads the kernel's IDT
(`arch::boot::interrupts`, `kernel/src/arch/x86_64/boot.rs`), the table in
`IDTR` is the one firmware left; `after_console` refuses `test-early-fault` by
name for that reason. AArch64 points `VBAR_EL1` at its own table before anything
can fault (`kernel/src/arch/aarch64/trap.rs`). A CPU exception in that window
(`pat::init`, `panic_console::arm`'s walk of the boot map, `mm::init`, the ACPI
parse) vectors through firmware's table to firmware's handlers. Until
`mm::init` the CPU is on the boot map, which holds firmware's memory
execute-disabled or not at all, so the handler's first fetch faults, that
fault's does too, and the machine resets; read from the map's encoding, not
observed on a machine. After `mm::init` the kernel's own tables hold the
same memory execute-disabled. No panic runs, so neither the panel nor serial
carries anything, and the reset clears what the panel showed.

What a machine shows today is at most location, never cause, and only until
the reset: the panel repaints on each record only until `params::init`, or
throughout on a boot naming `early-panel`. A fault's vector, `RIP` and `CR2`
reach no channel.

Exit: an exception table the kernel owns from its first statement, whose every
vector reaches the panic path — so a fault anywhere in the window is a panic
report on the panel and the console.
