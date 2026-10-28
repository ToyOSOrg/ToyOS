---
status: open
kind: track
opened: 2026-10-28
---

# AArch64 CPUs idle only in `wfi`

On x86 the kernel enters the states a processor's `_CST` names
(`kernel/src/idle.rs`); on AArch64 every idle CPU waits in `wfi`
(`kernel/src/arch/aarch64/hw.rs`), and `arch::idle::facts` admits nothing
deeper (`kernel/src/arch/aarch64/idle.rs`).

**The equivalent.** An Arm platform declares its idle states in `_LPI` under
its processor and processor-container devices (ACPI 6.5 §8.4.3), each entered
through PSCI `CPU_SUSPEND` with the `power_state` its FFH entry method names
(Arm DEN0022, DEN0048). A retention state comes back as `wfi` does. A
power-down state loses the core's context and needs a resume path. The
selection (`kernel::sched::idle`) and the `WakeLatency` bound are the
architecture's already; what is missing is the declaration's channel and
entry:

- AArch64 has no `acpi` claim or AML server to hand a declaration over
  (`kernel/src/arch/aarch64/acpi_mode.rs`);
- nothing decodes `_LPI` or processor containers, which claiming LPI support
  in `\_SB._OSC` asserts (§6.2.11.2, §8.4.3.5);
- no `CPU_SUSPEND` entry, and no always-on timer check (the GTDT's flag)
  standing for x86's ARAT.

No fleet machine runs AArch64 on metal, and whether QEMU's `virt` declares an
`_LPI` is unread.

**Exit**: an AArch64 machine whose firmware declares `_LPI` enters its
retention states through `CPU_SUSPEND`, chosen by the same selector, with a
metal or QEMU reading of the state entered and returned from. Power-down
states, with a resume path, follow once such a machine declares one.
