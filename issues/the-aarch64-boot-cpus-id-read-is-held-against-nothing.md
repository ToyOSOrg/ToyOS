---
status: open
kind: defect
opened: 2026-10-01
---

# The AArch64 boot CPU's id read is held against nothing

`Roster::commit` (`kernel/src/smp_roster.rs`) holds every AP's `arch::cpu::hardware_id`
against the MADT MPIDR it was started by, and x86-64's `boot_aps` holds the
boot CPU's against its LAPIC id. AArch64's `smp::start` makes the boot CPU's
read its roster slot (`ROSTER.set_bsp(me)`), and `irqchip::init` picks the
boot CPU's redistributor by it, so nothing independent of the read holds it.
A read naming no MADT CPU panics in `Gic::redistributor`. One naming another
CPU brings that CPU's redistributor up as the boot CPU's and skips that CPU,
then sends `CPU_ON` to the boot CPU, which PSCI refuses as already on, so
every CPU after it in the MADT stays off.

**Evidence:** the code (`kernel/src/arch/aarch64/smp.rs`,
`kernel/src/arch/aarch64/irqchip.rs`).

**Exit:** the boot CPU's read is held, before the roster takes it, against an
id it did not produce.
