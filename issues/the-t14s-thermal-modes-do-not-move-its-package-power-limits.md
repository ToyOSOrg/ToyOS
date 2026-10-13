---
status: open
kind: defect
opened: 2026-10-13
---

# The T14's thermal modes do not move its package power limits

The T14's hotkey device's `_INI` reaches Lenovo's thermal mode method
(`HKEY._INI` → `DYTC` → `SCPF` → `\_SB.PC00.PL1S` → `CTCN`), which writes
PL1 and PL2 as bytes of MCHBAR + 59A0h, `PACKAGE_RAPL_LIMIT` (Intel 631122
§3.3.31). That register is the kernel's: the performance track declares it
(`issues/the-kernel-owns-cpu-performance-state.md`, RAPL), so the kernel
refuses the write `KernelRegister` and the ACPI server answers it as made and
goes on (`toyos_userbound::firmware`, `system/acpiserver/src/host.rs`). The
method's other writes, the cTDP control at 5F50h and the turbo activation
ratio at 5F54h, are made, and `DYTC` records its mode in NVS.

So whatever mode the firmware selects, its PL1 and PL2 do not follow: the
firmware believes the limits moved, and the package runs under whatever the
kernel or the firmware's boot left in the register.

Owner: the performance track (the orchestrator).

## Exit condition

The performance track gives a firmware mode a way to move the kernel's
declared limits, and a T14 row reads PL1 back at the mode's value after
`DYTC`; or the owner rules that ToyOS does not follow the firmware's thermal
modes, and the T14's row says so.
