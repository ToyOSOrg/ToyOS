---
status: open
kind: defect
severity: [security]
opened: 2026-11-12
---

# Automatic IBRS stays off on an AMD CPU that has it

On an AMD CPU that enumerates automatic IBRS (APM Vol. 3
`CPUID Fn8000_0021_EAX[AutomaticIBRS]`, bit 8), `kernel::mitigation::decide`
names it for spectre_v2 and nothing sets `EFER.AIBRSE`:
`arch::x86_64::control_regs` declares `IA32_EFER` whole and constant. Such a
CPU runs the kernel's indirect branches unrestricted, and its spectre_v2
line says so. No proving machine enumerates it.

**Exit**: `control_regs`' declaration sets `AIBRSE` on each CPU whose
decision names automatic IBRS and asserts it there as it does the rest of
`IA32_EFER`; the words line gives `IA32_EFER`, and a boot on a machine that
enumerates it shows the bit on every CPU and its spectre_v2 line with
automatic IBRS in force. **Mutation**: the bit dropped from the declaration.
**Oracle**: the CPU's own register.
