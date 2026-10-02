---
status: open
kind: defect
opened: 2026-10-01
---

# A safe port read reaches any I/O port

`inb` and `inw` (`kernel/src/arch/x86_64/cpu.rs`) are safe `pub fn`s over
any port, "safe because a read has no value a caller can get wrong". A port
read has effects of its own: a read of COM1's data register takes the 16550's
received byte, a read of its line status clears the error bits, and a read of
the i8042's data port takes its output byte. So any kernel module takes a
received console byte with `crate::arch::cpu::inb(0x3f8)`, with no
`serial::Registers` and no `unsafe`, past the type that makes the UART's
registers their holder's alone. `outb` and `outw` are `unsafe`, under a
contract that the caller owns the port; the reads carry none.

**Evidence:** the code. `inb`'s callers are `arch::console_uart`, `arch::rtc`
and `arch::i8042`; `inw`'s is `arch::watchdog`.

**Exit:** no safe call reads a port it was not handed.
