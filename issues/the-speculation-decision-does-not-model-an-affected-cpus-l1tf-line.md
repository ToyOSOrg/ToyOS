---
status: open
kind: defect
opened: 2026-09-29
---

# The speculation decision does not model an affected CPU's l1tf line

`toyos_cpuvuln::Decision::line` answers `Unmodelled::L1tf` for a CPU with
`X86_BUG_L1TF`, because at `Ubuntu-6.8.0-142.142` that line rests on two
things `Facts` does not carry:

- **Whether RAM lies above `MAX_PA/2`**: `l1tf_select_mitigation` sets
  `L1TF_PTEINV` only if no e820 RAM range reaches `l1tf_pfn_limit()`
  (`bugs.c:2571-2583`; `asm/processor.h:217`), which is `x86_cache_bits` (CPUID 0x80000008:EAX,
  raised to 44 for the models `override_cache_bits` names, `bugs.c:2513-2536`)
  against the firmware's memory map. Without `L1TF_PTEINV` the line is
  "Vulnerable" (`bugs.c:3330-3333,3376`).
- **`kvm_intel`'s `l1tf_vmx_mitigation`**: the line's `VMX:` part is
  `VMENTER_L1D_FLUSH_AUTO` until `kvm_intel` loads (`bugs.c:3074-3089`), a
  module Linux's userspace loads, not a CPU fact.

The first is readable by the kernel (the loader's memory map, CPUID); the
second has no counterpart in a kernel that runs no VM, so parity needs a ruling
on which `VMX:` state ToyOS is held to.

No machine ToyOS is tested on reaches it: the T14 has `RDCL_NO`, and the TCG
model and the KVM runners are AMD.

**Exit**: the line is decided from the memory map and CPUID and the owner's
`VMX:` ruling, and held by a fixture captured under the pinned Linux on such a
CPU.
