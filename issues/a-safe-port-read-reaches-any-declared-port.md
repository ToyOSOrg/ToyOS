---
status: open
kind: defect
opened: 2026-10-01
---

# A safe port read reaches any declared port

`inb` and `inw` (`kernel/src/arch/x86_64/cpu.rs`) are safe `pub fn`s over
any `pio::Port`, "safe because a read has no value a caller can get wrong".
A `Port` is made only from a declared run or a mediated access
(`kernel/src/arch/x86_64/pio.rs`), but the fixed runs are `pub const`s, so any
kernel module reads one: `cpu::inb(pio::COM1.port(0))` takes the 16550's
received byte with no `serial::Registers` and no `unsafe`, past the type that
makes the UART's registers their holder's alone. A port read has effects of
its own: a read of COM1's data register takes the received byte, and a read of
its line status clears the error bits. `outb` and `outw` are `unsafe`, under a
contract that the caller owns what the write commands; the reads carry none.

**Evidence:** the code. `inb`'s callers are `arch::console_uart`, `arch::rtc`
and `arch::acpi_mode`; `inw`'s are `arch::acpi_mode`, `arch::power` and
`arch::watchdog`.

**Exit:** no safe call reads a port it was not handed.
