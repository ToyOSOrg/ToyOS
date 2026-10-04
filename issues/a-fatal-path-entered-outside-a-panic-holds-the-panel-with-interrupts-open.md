---
status: open
kind: defect
opened: 2026-10-03
---

# A fatal path entered outside a panic holds the panel with interrupts open

`panic::halt_all_cpus` says the machine holds its report "with interrupts
masked" and masks nothing itself: the panic handler and the exception entries
mask before they reach it. Two callers do not, both in the test kernel alone:
the `panel-painter-stalls` actuator, from a scheduler pass
(`kernel/src/drivers/panic_console/mod.rs`'s `stall`), and `SYS_DEBUG`'s
`FATAL_HALT` (`kernel/src/syscall/dispatch.rs`). The CPU that then holds the
panel spins on the reset bound with `IF` set, and every device handler pinned
to it still runs on a kernel that has declared itself dead.

**Evidence**: `screen_fatal_behind_a_painter` on `Profile::Metal` with two
CPUs, QEMU's `info registers -a` read once the report is on the panel: in six
boots of six the spinning CPU's `RFL` has bit 9 set (`0x293`, `0x287`, `0x297`)
and the halted one's has it clear. With `hold_the_panel` patched to poll port
0x60 and a key pressed inside the bound, six boots: the patched poll read the
key in four, each with the panel's CPU cpu1, and never saw it in two. The one
of those two whose console was kept had the panel on cpu0, where the i8042's
handler is pinned.

So that test stages a fatal path no shipping kernel takes. Not measured: that
the handler read the byte in the two boots the poll missed it; the open `IF`
on the handler's own CPU is what would let it.

**Exit**: `halt_all_cpus` masks interrupts itself, or its two callers outside
a fault do, and the same reading shows `IF` clear on the CPU holding the panel.
