---
status: open
kind: defect
opened: 2026-10-21
---

# x86-64's boot map is writable and executable

The loader's x86-64 boot map (`lib/bootmap/src/x86_64.rs`) maps the kernel's
text read-only and every other page writable, and every page executable: bit 63
is execute-disable only under `IA32_EFER.NXE`, and the loader switches to the
map from firmware's state, before the kernel declares `NXE`. The boot CPU runs
the kernel on it until `mm::init`, and each AP from its trampoline, whose page
at 0x8000 the map holds writable, until `smp::ap_entry` loads the kernel's
tables. The boot's audit (`mm::audit_wx`) walks the kernel's tables, not this
map. AArch64's boot map is not writable and executable anywhere: the kernel's
entry declares `SCTLR_EL1.WXN` before it turns translation on under it, and the
loader maps everything but the text execute-never.

Exit: as on AArch64, the kernel's entry applies its declared `EFER` before it
switches to the boot map; every leaf there but the text's and the trampoline
page's is execute-disable, and those two are read-only; and the plan's host
tests show it.
