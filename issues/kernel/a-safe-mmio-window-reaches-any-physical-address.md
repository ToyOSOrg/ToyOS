---
status: open
kind: defect
opened: 2026-10-01
---

# A safe `Mmio` window reaches any physical address

`Mmio::new` (`kernel/src/mm/mmio.rs`) is a safe `pub(crate)` constructor over
any `DirectMap`, `DirectMap::from_phys` (`kernel/src/mm/mod.rs`) is safe for
any address, and `Mmio::write_u32` is safe: any kernel module can write any
physical address, RAM included, with no `unsafe` to answer for. So no type
can make a device's registers its driver's alone: a module that builds a
window over `arch::console_uart::frame()` writes the PL011 without the
`serial::Registers` that `arch::console_uart::write_byte` asks for.

**Evidence:** the code. Its callers are `map_mmio` on both architectures, the
AArch64 GIC redistributor's window and the PL011's.

**Exit:** no safe call builds a window over memory it was not handed.
