---
status: open
kind: defect
opened: 2026-09-28
---

# The i8042's holder holds the machine's reset line

An `isa` claim on the i8042 (`kernel/src/arch/x86_64/pio.rs`'s `GRANTABLE`)
opens ports 0x60 and 0x64 to the process that binds it, through the TSS I/O
permission bitmap, and the bitmap grants a port or refuses it: it cannot see
the value written. The controller's command port takes `0xFE`, which pulses the
CPU's reset line, and `0xD1`, which writes its output port, where the same
line lives (IBM PC AT Technical Reference, the 8042's commands). So whichever
process drives the keyboard can reset the machine whenever it likes: no memory
is exposed and the kernel does not crash, but a userland bug in that one
program ends every other one without a word in the log.

Neither way out is free: filtering the command byte means the kernel decoding
the controller's protocol, a syscall or a trapped instruction per access in
place of the bitmap; keeping 0x64 in the kernel means the keyboard driver is
not wholly userland, which the owner ruled it must be.

A recorded weakness of the power broker's track
(`issues/isolation/the-power-broker-authority-with-a-human-in-the-loop.md`),
which owns it (owner ruling).

**Exit**: resetting the machine is the power broker's decision alone: the
keyboard's holder reaches the controller's reset line only through the broker,
or not at all.
