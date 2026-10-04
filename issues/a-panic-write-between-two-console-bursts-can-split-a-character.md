---
status: open
kind: defect
opened: 2026-10-01
---

# A panic-path write between two console bursts can split a character

`serial::uart_write_fifo` cuts a line into `TX_BURST`-byte bursts wherever the
count falls, so a multibyte character, such as the `—` many of the kernel's
records carry, can straddle two of them. The panic path takes the registers
between a wire holder's bursts (`panic_flush`'s drain, `nested_nmi`'s report),
so its bytes can land between that character's lead byte and the rest, and
the console line they cut is not UTF-8.

The harness's reader stops at the first console line that is not UTF-8
(`publish_line` in `tests/common/qemu.rs`), so the fatal report after it never
reaches an assertion, and the boot waits for QEMU to exit.

**Evidence:** the code. The harness's half was recorded with the unlocked
nested-NMI report as the splitter: main's nightly `guest` lane at `06788146b`,
run 36843762360, job 110374194368, where `nested_nmi_is_loud` ended `QEMU died
before NESTED NMI (status 0)` after 63 s; `PANIC_BOUND_MS` is 60 s. Its capture
stops in the line before the `—` of cpu1's `i8042:` record, and holds neither
the rest of the report nor the panic flush's records.

**Exit:** no console line the panic path writes into carries a split
character.
