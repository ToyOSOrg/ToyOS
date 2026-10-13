---
status: open
kind: defect
opened: 2026-10-12
---

# A panic before the reset register is read holds the machine for good

On x86-64 the kernel learns its reset from the FADT in `power::init_reset`,
which `arch::boot::interrupts` calls, well after `arch::boot::after_console`
(`kernel/src/main.rs`). A panic before it finds
`power::can_reboot` false, and `panic_reboot::arm` returns `Bound::Held`:
the panel is held until somebody cuts the power. On the AMD bench every panic
before `clock::init` (`arch::boot::clock`) is held as well, whatever the
reset: its CPU states no counter frequency (CPUID leaves 0x15 and 0x16), so
`panic_reboot::deadline` has nothing to time the bound with and `arm` takes
its "no clock" arm.

Everything `after_console` runs is in that window, the BSP's microcode load
among them. `issues/a-fatal-event-stands-down-both-bounds-and-may-leave-nothing-to-end-the-machine.md`
covers a later window, where a reset is known and both bounds stand down.

**Evidence.** #886's two negative-control arms panicked at the microcode
read-back inside `after_console`, and both benches had to be power-cycled by
hand: the T14 armed with `microcode-reseal` and the AMD bench with its
trigger write removed. The AMD bench's log partition, read after the power
cycle, carries the failed read-back; the T14's cannot tell the panic from a
fault at the trigger
(https://github.com/ToyOSOrg/ToyOS/pull/886#issuecomment-6131006022).

Owner: the orchestrator.

**Exit**: a T14 boot and an AMD bench boot armed with `test-early-panic`,
which panics right after `after_console`, each reset themselves with nobody
at the machine, and the boot after it reports the panic.
