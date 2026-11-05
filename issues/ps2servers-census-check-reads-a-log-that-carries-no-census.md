---
status: open
kind: finding
opened: 2026-11-05
---

# `ps2server::armed`'s census check reads a log that carries no census

`tests/common/ps2server.rs`'s `armed` refuses a census line whose sources are
not `tests/common/irqcensus.rs`'s `SOURCES`, so that no i8042 source sneaks
in. It reads the census off the boot's kernel log. On metal that is the log
partition, and the kernel logs its census at the stop, after `logkeeper`'s last
flush (`kernel/src/census.rs`), so the file never holds one and the check
passes over nothing there. Under QEMU the console carries the census and the
check reads it.

Owner: whoever next touches `ps2server_arms`.

**Exit**: the metal row reads a census the boot actually wrote, or the check
says on metal that it has nothing to read.
