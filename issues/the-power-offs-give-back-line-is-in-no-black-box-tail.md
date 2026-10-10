---
status: open
kind: defect
opened: 2026-10-08
---

# What the power-off's settle logs is committed after the black box's tail is sealed

`quiesce` (`kernel/src/syscall/machine.rs`) says the boot's last word, drains
the console and seals the newest records onto the black box's page
(`log::seal_tail`) before `power::shutdown` is entered. `shutdown` then calls
`arch::power::settle`, whose `acpi_mode::settle` logs `acpi: the Global Lock
given back for a holder that left it taken (the machine is stopping)` where a
holder was stopped holding the lock. That record is committed below the seal:
it is on the console, which `shutdown` drains after it, and in no page tail.
On the console it follows `Shutting down.`, which `quiesce` calls the boot's
last word.

So on a machine with no serial port no reader keeps the line: `/log`'s writer
was stopped with the rest of userland, the stop's last drain has no wire to drain to,
and the page was sealed without it. The T14 is such a machine, and one where
the kernel writes `ACPI_ENABLE` itself and a holder can be stopped holding
the lock.

What that costs today is narrow. A power-off that takes leaves nothing for a
next loader to read: S5 keeps no memory context by ACPI's definition (the
specification's, not a reading of the T14's DIMMs), and the page is DRAM. The
tail is read after a power-off only where the write did not take: `S5 did not
take`, a panic and then a reset.

Not moved in #767, which put the settle above the console's drain: the seal
is `quiesce`'s, shared by the reboot, which settles nothing; it stands above
`xhci::seal_shut`, which must be the last thing `quiesce` does, and the
settle needs the `Stopping` and runs below it. Sealing after the settle, or
settling above the last word, is a second change of the stop's order.

Owner: the `acpi` claim's author (#749).

**Exit**: no record is committed to the ring below `log::seal_tail` on either
end, read in `quiesce`, `power::shutdown` and `power::reboot`; and
`kernel/src/power.rs`'s header sentence then names the page beside the
console.
